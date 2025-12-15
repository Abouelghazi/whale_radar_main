// ============================================================================
// WhaleRadar – main.rs (Gestructureerd met Hoofdstukken & Secties)
// ============================================================================
//
// Overzicht:
//  0. Imports & dependencies
//  1. Configuratie & constantes
//  2. Bayesiaans AI / zelflerend systeem
//  3. Core data structuren
//  4. Scoring, signalen & output modellen
//  5. Virtuele trader (paper trading)
//  6. Engine (hart van het systeem)
//     6.1 Trade verwerking (WebSocket)
//     6.2 Ticker & anomaly verwerking (REST)
//     6.3 Analyse & snapshots
//     6.4 Trader integratie
//  7. Betrouwbaarheid & kwaliteitsscores
//  8. Normalisatie (assets & pairs)
//  9. Frontend (HTML dashboard)
// 10. WebSocket workers
// 11. REST anomaly scanner
// 12. Self-evaluator (zelflerend)
// 13. Cleanup & onderhoud
// 14. HTTP server & API
// 15. Main entrypoint
//
// Alleen comments toegevoegd — GEEN invloed op compilatie.
// ============================================================================

use chrono::Utc;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use tokio::time::{sleep, Duration};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use warp::Filter;

// ============================================================================
// HOOFDSTUK 1 – CONFIGURATIE & CONSTANTES
// ============================================================================

// ============================================================================
// HOOFDSTUK 2 – BAYESIAANS AI / ZELFLEREND SYSTEEM
// ============================================================================

use std::fs;

use chrono::DateTime;


const SIGNAL_FILE: &str = "signals.json";
const MAX_HISTORY: usize = 20;

const VIRTUAL_INITIAL_BALANCE: f64 = 10_000.0;
const VIRTUAL_BASE_NOTIONAL: f64 = 100.0;
const VIRTUAL_MAX_POSITIONS: usize = 5;
const VIRTUAL_SL_PCT: f64 = 0.02;
const VIRTUAL_TP_PCT: f64 = 0.05;


#[derive(Debug, Serialize, Deserialize, Clone)]
struct SignalStats {
    wins: u32,
    losses: u32,
    threshold: f64,
    last_updated: Option<DateTime<Utc>>,
    profit_history: Vec<f64>,
}

impl SignalStats {
    fn new(threshold: f64) -> Self {
        Self {
            wins: 0,
            losses: 0,
            threshold,
            last_updated: None,
            profit_history: Vec::new(),
        }
    }

    fn update(&mut self, profit: f64) {
        if profit > 0.0 { self.wins += 1; } else { self.losses += 1; }
        self.profit_history.push(profit);
        if self.profit_history.len() > MAX_HISTORY {
            self.profit_history.remove(0);
        }

        let total = (self.wins + self.losses) as f64;
        let p_success = (self.wins as f64 + 1.0) / (total + 2.0);
        let recent_avg: f64 = if !self.profit_history.is_empty() {
            self.profit_history.iter().sum::<f64>() / self.profit_history.len() as f64
        } else { 0.0 };

        if p_success > 0.7 && recent_avg > 0.0 && self.threshold > 0.1 {
            self.threshold -= 0.015;
        } else if p_success < 0.5 && recent_avg < 0.0 && self.threshold < 0.99 {
            self.threshold += 0.015;
        }

        self.threshold = self.threshold.clamp(0.1, 0.99);
        self.last_updated = Some(Utc::now());
        println!("[AI] Threshold {:.3} | success={:.2} | trend={:.4}", self.threshold, p_success, recent_avg);
    }
}

async fn load_signal_stats() -> HashMap<String, SignalStats> {
    match fs::read_to_string(SIGNAL_FILE) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

async fn save_signal_stats(map: &HashMap<String, SignalStats>) {
    if let Ok(json) = serde_json::to_string_pretty(map) {
        if let Err(e) = fs::write(SIGNAL_FILE, json) {
            eprintln!("[ERR] Kon signals.json niet opslaan: {}", e);
        }
    }
}


// ============================================================================
// HOOFDSTUK 3 – CORE DATA STRUCTUREN
// ============================================================================
// 3.1 TradeState   – realtime trades, flow, whales, pump-detectie
// 3.2 CandleState  – OHLC candles
// 3.3 TickerState  – REST ticker & anomaly info


#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TradeState {
    buy_volume: f64,
    sell_volume: f64,
    trade_count: u64,

    ewma_trade_size: Option<f64>,
    ewma_notional: Option<f64>,
    ewma_volume: Option<f64>,

    last_whale: bool,
    last_whale_side: Option<String>,
    last_whale_volume: Option<f64>,
    last_whale_notional: Option<f64>,

    last_early: Option<String>,
    last_alpha: Option<String>,
    last_score: f64,
    last_rating: Option<String>,

    last_flow_pct: f64,
    last_dir: String,
    recent_buys: Vec<(f64, f64)>,
    recent_sells: Vec<(f64, f64)>,

    // 5-min flow
    recent_buys_5m: Vec<(f64, f64)>,
    recent_sells_5m: Vec<(f64, f64)>,
    last_flow_pct_5m: f64,
    last_dir_5m: String,

    // pump detection
    recent_prices: Vec<(f64, f64)>, // (ts, price)
    last_pump_score: f64,
    last_pump_signal: Option<String>,

    // whale prediction
    whale_pred_score: f64,
    whale_pred_label: Option<String>,

    last_update_ts: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CandleState {
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    close: Option<f64>,
    pct_change: Option<f64>,

    first_ts: Option<i64>,
    last_ts: Option<i64>,

    last_update_ts: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TickerState {
    last_price: Option<f64>,
    last_vol24h: Option<f64>,
    ewma_vol24h: Option<f64>,
    ewma_abs_return: Option<f64>,

    last_anom_ts: Option<i64>,
    last_anom_dir: Option<String>,
    last_anom_strength: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OrderbookState {
    bids: Vec<(f64, f64)>,  // (price, volume)
    asks: Vec<(f64, f64)>,  // (price, volume)
    timestamp: i64,
}

#[derive(Debug, Clone, Serialize)]
struct Row {
    pair: String,
    price: f64,
    pct: f64,
    whale: bool,
    whale_side: String,
    whale_volume: f64,
    whale_notional: f64,
    flow_pct: f64,
    dir: String,
    early: String,
    alpha: String,
    pump_score: f64,
    pump_label: String,
    trades: u64,
    buys: f64,
    sells: f64,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    score: f64,
    rating: String,

    // whale prediction fields
    whale_pred_score: f64,
    whale_pred_label: String,

    // reliability fields
    reliability_score: f64,
    reliability_label: String,
}

#[derive(Debug, Clone)]
struct ScoreWeights {
    flow_w: f64,
    price_w: f64,
    whale_w: f64,
    volume_w: f64,
    anomaly_w: f64,
    trend_w: f64,
}
impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            // Iets meer nadruk op flow/volume/anomaly, iets minder op pure prijs
            flow_w: 2.2,
            price_w: 0.7,
            whale_w: 1.4,
            volume_w: 1.3,
            anomaly_w: 1.5,
            trend_w: 1.1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SignalEvent {
    ts: i64,
    pair: String,
    signal_type: String,
    direction: String,
    strength: f64,
    flow_pct: f64,
    pct: f64,
    whale: bool,
    whale_side: String,
    volume: f64,
    notional: f64,
    price: f64,

    rating: String,
    total_score: f64,
    flow_score: f64,
    price_score: f64,
    whale_score: f64,
    volume_score: f64,
    anomaly_score: f64,
    trend_score: f64,
    evaluated: bool,

    // backtest: gerealiseerde performance over ~5m
    ret_5m: Option<f64>,
    eval_horizon_sec: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
struct TopRow {
    ts: i64,
    pair: String,
    price: f64,
    pct: f64,
    flow_pct: f64,
    dir: String,
    early: String,
    alpha: String,
    pump_score: f64,
    pump_label: String,
    whale: bool,
    whale_side: String,
    whale_volume: f64,
    whale_notional: f64,
    total_score: f64,
    analysis: String,

    whale_pred_score: f64,
    whale_pred_label: String,

    // reliability fields
    reliability_score: f64,
    reliability_label: String,
}

#[derive(Debug, Clone, Serialize)]
struct Top10Response {
    best3: Vec<TopRow>,
    risers: Vec<TopRow>,
    fallers: Vec<TopRow>,
}

#[derive(Debug, Clone, Serialize)]
struct HeatmapPoint {
    pair: String,
    flow_pct: f64,
    pump_score: f64,
    ts: i64,
}

#[derive(Debug, Clone, Serialize)]
struct BacktestResult {
    signal_type: String,
    direction: String,

    total_trades: usize,
    winrate: f64,
    avg_win: f64,
    avg_loss: f64,
    expectancy: f64,
    pnl_sum: f64,

    max_drawdown: f64,
    best_trade: f64,
    worst_trade: f64,
    max_losing_streak: usize,

    equity_curve: Vec<f64>,
}

// ============================================================================
// HOOFDSTUK 5 – VIRTUELE TRADER (PAPER TRADING)
// ============================================================================


#[derive(Debug, Clone, Serialize)]
struct TradeAdviceRow {
    pair: String,
    price: f64,
    entry_price: f64,
    exit_5: f64,
    exit_10: f64,
    exit_15: f64,
    exit_20: f64,
}

#[derive(Debug, Clone, Serialize)]
struct TradeAdviceEquity {
    equity_5: f64,
    equity_10: f64,
    equity_15: f64,
    equity_20: f64,
}

#[derive(Debug, Clone, Serialize)]
struct TradeAdviceResponse {
    rows: Vec<TradeAdviceRow>,
    equity: TradeAdviceEquity,
}

#[derive(Debug, Clone)]
struct Position {
    pair: String,
    entry_price: f64,
    size: f64,
    open_ts: i64,
    stop_loss: f64,
    take_profit: f64,
}

#[derive(Debug)]
struct TraderState {
    initial_balance: f64,
    balance: f64,
    base_notional: f64,
    max_positions: usize,
    positions: HashMap<String, Position>,
    realized_pnl: f64,
}

impl TraderState {
    fn new() -> Self {
        Self {
            initial_balance: VIRTUAL_INITIAL_BALANCE,
            balance: VIRTUAL_INITIAL_BALANCE,
            base_notional: VIRTUAL_BASE_NOTIONAL,
            max_positions: VIRTUAL_MAX_POSITIONS,
            positions: HashMap::new(),
            realized_pnl: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct PositionView {
    pair: String,
    entry_price: f64,
    size: f64,
    open_ts: i64,
    stop_loss: f64,
    take_profit: f64,
    current_price: f64,
    pnl_abs: f64,
    pnl_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
struct PositionsResponse {
    balance: f64,
    realized_pnl: f64,
    unrealized_pnl: f64,
    positions: Vec<PositionView>,
}

// ============================================================================
// HOOFDSTUK 6 – ENGINE (HART VAN HET SYSTEEM)
// ============================================================================


#[derive(Clone)]
struct Engine {
trades: Arc<DashMap<String, TradeState>>,
    candles: Arc<DashMap<String, CandleState>>,
    tickers: Arc<DashMap<String, TickerState>>,
    orderbooks: Arc<DashMap<String, OrderbookState>>,
    signals: Arc<Mutex<Vec<SignalEvent>>>,
    signalled_pairs: Arc<DashMap<String, bool>>,
    weights: Arc<Mutex<ScoreWeights>>,

    trader: Arc<Mutex<TraderState>>, 
}

impl Engine {
        fn new() -> Self {
        Self {
            trades: Arc::new(DashMap::new()),
            candles: Arc::new(DashMap::new()),
            tickers: Arc::new(DashMap::new()),
            orderbooks: Arc::new(DashMap::new()),
            signals: Arc::new(Mutex::new(Vec::new())),
            signalled_pairs: Arc::new(DashMap::new()),
            weights: Arc::new(Mutex::new(ScoreWeights::default())),
            trader: Arc::new(Mutex::new(TraderState::new())),
        }
    }

    fn mark_signalled(&self, pair: &str) {
        self.signalled_pairs.insert(pair.to_string(), true);
    }

    fn push_signal(&self, ev: SignalEvent) {
        self.mark_signalled(&ev.pair);
        self.maybe_open_position_from_signal(&ev);
        let mut buf = self.signals.lock().unwrap();
        buf.push(ev);
        if buf.len() > 400 {
            let overflow = buf.len() - 400;
            buf.drain(0..overflow);
        }
    }

    // -------------------------------------------------------------------------
    // TRADE HANDLER
    // -------------------------------------------------------------------------

    fn handle_trade(&self, pair: &str, price: f64, volume: f64, side: &str, ts: f64) {
        let ts_int = ts.floor() as i64;
        let mut t = self.trades.entry(pair.to_string()).or_default();

        let prev_whale = t.last_whale;
        let prev_early = t
            .last_early
            .clone()
            .unwrap_or_else(|| "NONE".to_string());
        let prev_alpha = t
            .last_alpha
            .clone()
            .unwrap_or_else(|| "NONE".to_string());
        let prev_pump_sig = t
            .last_pump_signal
            .clone()
            .unwrap_or_else(|| "NONE".to_string());
        let prev_pred_label = t
            .whale_pred_label
            .clone()
            .unwrap_or_else(|| "NONE".to_string());

        t.last_update_ts = ts_int;

        // volumes
        if side == "b" {
            t.buy_volume += volume;
        } else {
            t.sell_volume += volume;
        }
        t.trade_count += 1;

        let notional = price * volume;

        // EWMA size
        let s0 = t.ewma_trade_size.unwrap_or(volume);
        let s1 = 0.9 * s0 + 0.1 * volume;
        t.ewma_trade_size = Some(s1);

        // EWMA notional
        let n0 = t.ewma_notional.unwrap_or(notional);
        let n1 = 0.9 * n0 + 0.1 * notional;
        t.ewma_notional = Some(n1);

        // EWMA volume
        let v0 = t.ewma_volume.unwrap_or(volume);
        let v1 = 0.9 * v0 + 0.1 * volume;
        t.ewma_volume = Some(v1);

        // Whale detectie
        let min_notional = 5_000.0_f64;
        let is_whale = notional > min_notional && notional > n1 * 2.5;
        if is_whale {
            t.last_whale = true;
            t.last_whale_side = Some(side.to_string());
            t.last_whale_volume = Some(volume);
            t.last_whale_notional = Some(notional);
        } else {
            t.last_whale = false;
            t.last_whale_side = None;
            t.last_whale_volume = None;
            t.last_whale_notional = None;
        }

        // Candle update
        let mut c = self.candles.entry(pair.to_string()).or_default();
        c.last_update_ts = ts_int;

        if c.open.is_none() {
            c.open = Some(price);
            c.high = Some(price);
            c.low = Some(price);
            c.close = Some(price);
            c.first_ts = Some(ts_int);
            c.last_ts = Some(ts_int);
            c.pct_change = Some(0.0);
        } else {
            c.high = Some(c.high.unwrap().max(price));
            c.low = Some(c.low.unwrap().min(price));
            c.close = Some(price);
            c.last_ts = Some(ts_int);
            let o = c.open.unwrap();
            c.pct_change = Some(((price - o) / o) * 100.0);
        }

        let pct = c.pct_change.unwrap_or(0.0);

        // price history for pump detection
        t.recent_prices.push((ts, price));
        let cutoff_price = ts - 300.0;
        t.recent_prices.retain(|(x, _)| *x >= cutoff_price);

        // -------------------- Flow (60s) --------------------
        let cutoff = ts - 60.0;
        if side == "b" {
            t.recent_buys.push((ts, volume));
        } else {
            t.recent_sells.push((ts, volume));
        }
        t.recent_buys.retain(|(x, _)| *x >= cutoff);
        t.recent_sells.retain(|(x, _)| *x >= cutoff);

        let b: f64 = t.recent_buys.iter().map(|(_, v)| *v).sum();
        let s: f64 = t.recent_sells.iter().map(|(_, v)| *v).sum();
        let tot = b + s;

        let (flow_pct, dir) = if tot > 0.0 {
            let f = b / tot;
            if f > 0.75 {
                (f * 100.0, "BUY".to_string())
            } else if f < 0.25 {
                ((1.0 - f) * 100.0, "SELL".to_string())
            } else {
                (50.0, "NEUTR".to_string())
            }
        } else {
            (50.0, "NEUTR".to_string())
        };

        t.last_flow_pct = flow_pct;
        t.last_dir = dir.clone();

        // -------------------- Flow (5 min) --------------------
        let cutoff5 = ts - 300.0;
        if side == "b" {
            t.recent_buys_5m.push((ts, volume));
        } else {
            t.recent_sells_5m.push((ts, volume));
        }
        t.recent_buys_5m.retain(|(x, _)| *x >= cutoff5);
        t.recent_sells_5m.retain(|(x, _)| *x >= cutoff5);

        let b5: f64 = t.recent_buys_5m.iter().map(|(_, v)| *v).sum();
        let s5: f64 = t.recent_sells_5m.iter().map(|(_, v)| *v).sum();
        let tot5 = b5 + s5;

        let (flow_pct_5m, dir_5m) = if tot5 > 0.0 {
            let f = b5 / tot5;
            if f > 0.70 {
                (f * 100.0, "BUY".to_string())
            } else if f < 0.30 {
                ((1.0 - f) * 100.0, "SELL".to_string())
            } else {
                (50.0, "NEUTR".to_string())
            }
        } else {
            (50.0, "NEUTR".to_string())
        };

        t.last_flow_pct_5m = flow_pct_5m;
        t.last_dir_5m = dir_5m.clone();

        // -------------------- Anomaly info uit Ticker --------------------
        let (anom_strength, has_recent_anom) = {
            if let Some(tk) = self.tickers.get(pair) {
                let strength = tk.last_anom_strength.unwrap_or(0.0);
                if let Some(at) = tk.last_anom_ts {
                    let age = ts_int.saturating_sub(at);
                    if age >= 0 && age <= 600 {
                        (strength, true)
                    } else {
                        (0.0, false)
                    }
                } else {
                    (0.0, false)
                }
            } else {
                (0.0, false)
            }
        };

        // -------------------- Scores --------------------
        // Flow short
        let mut flow_score_short = 0.0;
        if flow_pct > 75.0 {
            flow_score_short = 3.0;
        } else if flow_pct > 65.0 {
            flow_score_short = 2.0;
        } else if flow_pct > 55.0 {
            flow_score_short = 1.0;
        }

        // Flow long
        let mut flow_score_long = 0.0;
        if dir_5m == "BUY" && flow_pct_5m > 75.0 {
            flow_score_long = 2.0;
        } else if dir_5m == "BUY" && flow_pct_5m > 65.0 {
            flow_score_long = 1.0;
        }

        let mut flow_score = flow_score_short + 0.5 * flow_score_long;
        if flow_score > 3.0 {
            flow_score = 3.0;
        }

        let mut price_score = 0.0;
        if pct > 2.0 {
            price_score = 3.0;
        } else if pct > 1.0 {
            price_score = 2.0;
        } else if pct > 0.3 {
            price_score = 1.0;
        }

        let mut whale_score = 0.0;
        if is_whale {
            if notional > 50_000.0 || notional > n1 * 6.0 {
                whale_score = 3.0;
            } else if notional > 20_000.0 && notional > n1 * 4.0 {
                whale_score = 2.0;
            } else {
                whale_score = 1.0;
            }
        }

        // -------------------- Orderbook analysis --------------------
        // Adjust whale_score based on orderbook walls (buy/sell pressure)
        if let Some(ob) = self.orderbooks.get(pair) {
            let age = ts_int.saturating_sub(ob.timestamp);
            // Only use recent orderbook data (within 10 seconds)
            if age >= 0 && age <= 10 {
                // Calculate total volume in top bids and asks
                let bid_volume: f64 = ob.bids.iter().take(10).map(|(_, v)| v).sum();
                let ask_volume: f64 = ob.asks.iter().take(10).map(|(_, v)| v).sum();
                let total_volume = bid_volume + ask_volume;

                if total_volume > 0.0 {
                    let bid_ratio = bid_volume / total_volume;
                    
                    // If there's a large buy wall (high bid volume) and we're seeing buy trades
                    if side == "b" && bid_ratio > 0.65 {
                        whale_score += 0.5; // Boost score for buy pressure
                    }
                    // If there's a large sell wall (high ask volume) and we're seeing sell trades
                    else if side == "s" && bid_ratio < 0.35 {
                        whale_score += 0.5; // Boost score for sell pressure
                    }

                    // Additional boost for extreme imbalances
                    if bid_ratio > 0.75 && side == "b" {
                        whale_score += 0.3;
                    } else if bid_ratio < 0.25 && side == "s" {
                        whale_score += 0.3;
                    }
                }
            }
        }

        // Clamp whale_score to reasonable range
        if whale_score > 4.0 {
            whale_score = 4.0;
        }

        let mut volume_score = 0.0;
        let vol_ratio = if v1 > 0.0 { volume / v1 } else { 1.0 };
        if vol_ratio > 2.5 {
            volume_score = 3.0;
        } else if vol_ratio > 1.5 {
            volume_score = 2.0;
        } else if vol_ratio > 1.2 {
            volume_score = 1.0;
        }

        let mut anomaly_score = 0.0;
        if has_recent_anom {
            if anom_strength > 80.0 {
                anomaly_score = 3.0;
            } else if anom_strength > 40.0 {
                anomaly_score = 2.0;
            } else if anom_strength > 0.0 {
                anomaly_score = 1.0;
            }
        }

        let mut trend_score = 0.0;
        if is_whale && side == "b" && pct > 0.0 && flow_pct > 60.0 {
            trend_score += 1.0;
        }

        // -------------------- Advanced Pump Detector --------------------
        let mut ret_5s = 0.0_f64;
        let mut ret_30s = 0.0_f64;
        let mut ret_120s = 0.0_f64;

        for (pt, p_old) in t.recent_prices.iter() {
            let age = ts - *pt;
            if *p_old > 0.0 && price > 0.0 {
                if age >= 5.0 && age <= 7.0 {
                    ret_5s = (price - *p_old) / *p_old * 100.0;
                }
                if age >= 30.0 && age <= 40.0 {
                    ret_30s = (price - *p_old) / *p_old * 100.0;
                }
                if age >= 110.0 && age <= 130.0 {
                    ret_120s = (price - *p_old) / *p_old * 100.0;
                }
            }
        }

        if ret_5s < 0.0 {
            ret_5s = 0.0;
        }
        if ret_30s < 0.0 {
            ret_30s = 0.0;
        }
        if ret_120s < 0.0 {
            ret_120s = 0.0;
        }

        let mut pump_score = 0.0_f64;

        if ret_5s > 0.3 {
            pump_score += (ret_5s - 0.3) * 2.0;
        }
        if ret_30s > 1.0 {
            pump_score += (ret_30s - 1.0) * 1.0;
        }
        if ret_120s > 2.0 {
            pump_score += (ret_120s - 2.0) * 0.5;
        }
        if dir == "BUY" && flow_pct > 65.0 {
            pump_score += (flow_pct - 65.0) * 0.08;
        }
        if dir_5m == "BUY" && flow_pct_5m > 60.0 {
            pump_score += (flow_pct_5m - 60.0) * 0.06;
        }
        if vol_ratio > 1.5 {
            pump_score += (vol_ratio - 1.5) * 1.0;
        }
        if whale_score > 0.0 {
            pump_score += whale_score * 0.7;
        }

        if pump_score < 0.0 {
            pump_score = 0.0;
        }
        if pump_score > 10.0 {
            pump_score = 10.0;
        }

        t.last_pump_score = pump_score;

        let mut pump_conf = 0.0_f64;
        if ret_5s > 0.5 {
            pump_conf += 0.4;
        }
        if ret_30s > 1.5 {
            pump_conf += 0.3;
        }
        if ret_120s > 3.0 {
            pump_conf += 0.2;
        }
        if dir == "BUY" && flow_pct > 70.0 {
            pump_conf += 0.3;
        }
        if dir_5m == "BUY" && flow_pct_5m > 65.0 {
            pump_conf += 0.2;
        }
        if vol_ratio > 2.0 {
            pump_conf += 0.2;
        }
        if whale_score >= 2.0 {
            pump_conf += 0.2;
        }

        let mut pump_label = "NONE".to_string();
        if pump_score >= 7.0 && pump_conf >= 0.9 && dir == "BUY" {
            pump_label = "MEGA_PUMP".to_string();
        } else if pump_score >= 4.0 && pump_conf >= 0.5 && dir == "BUY" {
            pump_label = "EARLY_PUMP".to_string();
        }
        t.last_pump_signal = Some(pump_label.clone());

        // totale score
        let weights = self.weights.lock().unwrap().clone();
        let total_score = weights.flow_w * flow_score
            + weights.price_w * price_score
            + weights.whale_w * whale_score
            + weights.volume_w * volume_score
            + weights.anomaly_w * anomaly_score
            + weights.trend_w * trend_score;

        // Gevoeligere drempels zodat EARLY/BUY eerder afgaan
        let rating = if total_score >= 7.5 {
            "ALPHA BUY".to_string()
        } else if total_score >= 5.0 {
            "STRONG BUY".to_string()
        } else if total_score >= 3.5 {
            "BUY".to_string()
        } else if total_score >= 2.2 {
            "EARLY BUY".to_string()
        } else {
            "NONE".to_string()
        };

        t.last_score = total_score;
        t.last_rating = Some(rating.clone());

        // -------------------- Whale Prediction Engine --------------------
        let mut whale_pred_score = 0.0;

        // Sterke kortetermijn BUY-flow zonder dat er al een whale-print is
        if !is_whale && dir == "BUY" && flow_pct > 60.0 {
            whale_pred_score += (flow_pct - 60.0) * 0.08;
        }

        // Consistente 5m BUY-accumulatie
        if !is_whale && dir_5m == "BUY" && flow_pct_5m > 55.0 {
            whale_pred_score += (flow_pct_5m - 55.0) * 0.06;
        }

        // Kleine trade t.o.v. typische size -> stealth accumulation
        if !is_whale && volume < s1 * 0.8 {
            whale_pred_score += 1.0;
        }

        // Rustige prijs terwijl flow oploopt -> stille accumulatie
        let abs_ret_5s = ret_5s.abs();
        let abs_ret_30s = ret_30s.abs();
        if abs_ret_5s < 0.5 && abs_ret_30s < 1.0 && pct >= -0.5 {
            whale_pred_score += 1.0;
        }

        // Volume nog niet extreem -> nog in opbouwfase
        if vol_ratio < 1.3 {
            whale_pred_score += 0.5;
        }

        if whale_pred_score < 0.0 {
            whale_pred_score = 0.0;
        }
        if whale_pred_score > 10.0 {
            whale_pred_score = 10.0;
        }

        let whale_pred_label = if whale_pred_score >= 7.0 {
            "HIGH"
        } else if whale_pred_score >= 4.0 {
            "MEDIUM"
        } else if whale_pred_score >= 2.0 {
            "LOW"
        } else {
            "NONE"
        }
        .to_string();

        t.whale_pred_score = whale_pred_score;
        t.whale_pred_label = Some(whale_pred_label.clone());

        // WH_PRED signaal bij overgang naar HIGH
        if whale_pred_label == "HIGH" && prev_pred_label != "HIGH" {
            let ev = SignalEvent {
                ts: ts_int,
                pair: pair.to_string(),
                signal_type: "WH_PRED".to_string(),
                direction: "BUY".to_string(),
                strength: whale_pred_score,
                flow_pct,
                pct,
                whale: is_whale,
                whale_side: side.to_string(),
                volume,
                notional,
                price,
                rating: rating.clone(),
                total_score,
                flow_score,
                price_score,
                whale_score,
                volume_score,
                anomaly_score,
                trend_score,
                evaluated: false,
                ret_5m: None,
                eval_horizon_sec: None,
            };
            self.push_signal(ev);
        }

        // Early / Alpha flags
        let mut new_early = "NONE".to_string();
        let mut new_alpha = "NONE".to_string();

        if dir == "BUY" {
            if rating == "EARLY BUY" || rating == "BUY" {
                new_early = "BUY".to_string();
            } else if rating == "STRONG BUY" || rating == "ALPHA BUY" {
                new_early = "BUY".to_string();
                new_alpha = "BUY".to_string();
            }
        }

        t.last_early = Some(new_early.clone());
        t.last_alpha = Some(new_alpha.clone());

        // -------------------- Signals --------------------
        // Pump (Early / Mega)
        if pump_label != "NONE" && pump_label != prev_pump_sig {
            let ev = SignalEvent {
                ts: ts_int,
                pair: pair.to_string(),
                signal_type: pump_label.clone(),
                direction: "BUY".to_string(),
                strength: pump_score,
                flow_pct,
                pct,
                whale: is_whale,
                whale_side: side.to_string(),
                volume,
                notional,
                price,
                rating: rating.clone(),
                total_score,
                flow_score,
                price_score,
                whale_score,
                volume_score,
                anomaly_score,
                trend_score,
                evaluated: false,
                ret_5m: None,
                eval_horizon_sec: None,
            };
            self.push_signal(ev);
        }

        // Whale
        if is_whale && !prev_whale {
            let ev = SignalEvent {
                ts: ts_int,
                pair: pair.to_string(),
                signal_type: "WHALE".to_string(),
                direction: if side == "b" {
                    "BUY".to_string()
                } else {
                    "SELL".to_string()
                },
                strength: notional,
                flow_pct,
                pct,
                whale: true,
                whale_side: side.to_string(),
                volume,
                notional,
                price,
                rating: rating.clone(),
                total_score,
                flow_score,
                price_score,
                whale_score,
                volume_score,
                anomaly_score,
                trend_score,
                evaluated: false,
                ret_5m: None,
                eval_horizon_sec: None,
            };
            self.push_signal(ev);
        }

        // Early
        if new_early != "NONE" && new_early != prev_early {
            let ev = SignalEvent {
                ts: ts_int,
                pair: pair.to_string(),
                signal_type: "EARLY".to_string(),
                direction: new_early.clone(),
                strength: total_score,
                flow_pct,
                pct,
                whale: is_whale,
                whale_side: side.to_string(),
                volume,
                notional,
                price,
                rating: rating.clone(),
                total_score,
                flow_score,
                price_score,
                whale_score,
                volume_score,
                anomaly_score,
                trend_score,
                evaluated: false,
                ret_5m: None,
                eval_horizon_sec: None,
            };
            self.push_signal(ev);
        }

        // Alpha
        if new_alpha != "NONE" && new_alpha != prev_alpha {
            let ev = SignalEvent {
                ts: ts_int,
                pair: pair.to_string(),
                signal_type: "ALPHA".to_string(),
                direction: new_alpha.clone(),
                strength: total_score,
                flow_pct,
                pct,
                whale: is_whale,
                whale_side: side.to_string(),
                volume,
                notional,
                price,
                rating: rating.clone(),
                total_score,
                flow_score,
                price_score,
                whale_score,
                volume_score,
                anomaly_score,
                trend_score,
                evaluated: false,
                ret_5m: None,
                eval_horizon_sec: None,
            };
            self.push_signal(ev);
        }
            self.handle_price_for_trader(pair, price, ts_int);
}

    // -------------------------------------------------------------------------
    // TICKER / ANOMALY HANDLER
    // -------------------------------------------------------------------------

    fn handle_ticker(&self, pair: &str, last: f64, vol24h: f64, open: f64, ts_int: i64) {
        let mut ts = self.tickers.entry(pair.to_string()).or_default();

        let prev_price = ts.last_price.unwrap_or(last);
        let prev_vol = ts.last_vol24h.unwrap_or(vol24h);

        let day_ret = if open > 0.0 {
            (last - open) / open * 100.0
        } else {
            0.0
        };

        let jump = if prev_price > 0.0 {
            ((last - prev_price) / prev_price).abs() * 100.0
        } else {
            0.0
        };

        let vol_ratio = if prev_vol > 0.0 {
            vol24h / prev_vol.max(1e-9)
        } else {
            1.0
        };

        // EWMA volume en abs-return
        let ew_vol0 = ts.ewma_vol24h.unwrap_or(vol24h);
        let ew_vol1 = 0.9 * ew_vol0 + 0.1 * vol24h;
        ts.ewma_vol24h = Some(ew_vol1);

        let ew_ret0 = ts.ewma_abs_return.unwrap_or(jump);
        let ew_ret1 = 0.9 * ew_ret0 + 0.1 * jump;
        ts.ewma_abs_return = Some(ew_ret1);

        ts.last_price = Some(last);
        ts.last_vol24h = Some(vol24h);

        // Candle presence
        {
            let mut t = self.trades.entry(pair.to_string()).or_default();
            t.last_update_ts = ts_int;

            let mut c = self.candles.entry(pair.to_string()).or_default();
            c.last_update_ts = ts_int;

            if c.open.is_none() {
                c.open = Some(open);
                c.high = Some(last);
                c.low = Some(last);
                c.close = Some(last);
                c.first_ts = Some(ts_int);
                c.last_ts = Some(ts_int);
                c.pct_change = Some(((last - open) / open) * 100.0);
            } else {
                c.close = Some(last);
                c.high = Some(c.high.unwrap().max(last));
                c.low = Some(c.low.unwrap().min(last));
                c.last_ts = Some(ts_int);
                if let Some(o) = c.open {
                    c.pct_change = Some(((last - o) / o) * 100.0);
                }
            }
        }

        // anomaly-score
        let mut score = 0.0;
        score += jump * 2.0;
        score += day_ret.abs() * 0.5;
        if vol_ratio > 1.0 {
            score += (vol_ratio - 1.0) * 20.0;
        }
        score += ts.ewma_abs_return.unwrap_or(jump);

        if score > 40.0 && (jump > 0.3 || vol_ratio > 2.0) {
            let direction = if last >= prev_price { "BUY" } else { "SELL" };

            ts.last_anom_ts = Some(ts_int);
            ts.last_anom_dir = Some(direction.to_string());
            ts.last_anom_strength = Some(score);

            let ev = SignalEvent {
                ts: ts_int,
                pair: pair.to_string(),
                signal_type: "ANOM".to_string(),
                direction: direction.to_string(),
                strength: score,
                flow_pct: 0.0,
                pct: day_ret,
                whale: false,
                whale_side: "-".to_string(),
                volume: 0.0,
                notional: 0.0,
                price: last,
                rating: "NONE".to_string(),
                total_score: 0.0,
                flow_score: 0.0,
                price_score: 0.0,
                whale_score: 0.0,
                volume_score: 0.0,
                anomaly_score: 0.0,
                trend_score: 0.0,
                evaluated: true,
                ret_5m: None,
                eval_horizon_sec: None,
            };
            self.push_signal(ev);
        }
    }

    // -------------------------------------------------------------------------
    // SNAPSHOTS / TOP10 / HEATMAP / BACKTEST / TRADE ADVICE
    // -------------------------------------------------------------------------

    
    fn compute_reliability(t: &TradeState, now_ts: i64) -> (f64, String) {
        let now_f = now_ts as f64;

        // Kijk naar activiteit in de laatste 60s en 300s
        let cutoff_60 = now_f - 60.0;
        let cutoff_300 = now_f - 300.0;

        let mut recent_trades_60: usize = 0;
        let mut vol_60: f64 = 0.0;
        for (ts, v) in t.recent_buys.iter().chain(t.recent_sells.iter()) {
            if *ts >= cutoff_60 {
                recent_trades_60 += 1;
                vol_60 += *v;
            }
        }

        let mut vol_300: f64 = 0.0;
        for (ts, v) in t.recent_buys_5m.iter().chain(t.recent_sells_5m.iter()) {
            if *ts >= cutoff_300 {
                vol_300 += *v;
            }
        }

        // 1) Trade density score (0–40): hoeveel trades in laatste 60s?
        let td = (recent_trades_60.min(30) as f64 / 30.0) * 40.0;

        // 2) Volume stability (0–20): extreme spikes zijn minder betrouwbaar
        let ew_v = t.ewma_volume.unwrap_or(vol_300.max(1e-9));
        let vol_ratio = if ew_v > 0.0 { vol_300 / ew_v } else { 1.0 };

        let vs = if vol_ratio > 4.0 {
            0.0
        } else if vol_ratio > 2.0 {
            10.0
        } else {
            20.0
        };

        // 3) Flow consistency (0–20): hoge BUY/SELL-dominantie mét volume
        let mut buys_60: f64 = 0.0;
        let mut sells_60: f64 = 0.0;
        for (ts, v) in t.recent_buys.iter() {
            if *ts >= cutoff_60 {
                buys_60 += *v;
            }
        }
        for (ts, v) in t.recent_sells.iter() {
            if *ts >= cutoff_60 {
                sells_60 += *v;
            }
        }
        let tot_60 = buys_60 + sells_60;
        let flow_pct_60 = if tot_60 > 0.0 {
            buys_60 / tot_60 * 100.0
        } else {
            50.0
        };

        let fc = if tot_60 < 1.0 {
            // bijna geen volume → onbetrouwbaar
            0.0
        } else if flow_pct_60 > 70.0 || flow_pct_60 < 30.0 {
            20.0
        } else {
            15.0
        };

        // 4) Recency (0–15): hoe oud is de laatste update?
        let dt = now_ts.saturating_sub(t.last_update_ts);
        let ras = if dt > 300 {
            0.0
        } else if dt > 120 {
            5.0
        } else if dt > 60 {
            10.0
        } else {
            15.0
        };

        // 5) Time-density (0–15): zijn trades verspreid in de tijd?
        let tds = if recent_trades_60 >= 20 {
            15.0
        } else if recent_trades_60 >= 5 {
            8.0
        } else {
            0.0
        };

        let mut score = td + vs + fc + ras + tds;
        if score > 100.0 {
            score = 100.0;
        }

        let label = if score <= 25.0 {
            "UNRELIABLE"
        } else if score <= 50.0 {
            "LOW"
        } else if score <= 75.0 {
            "MEDIUM"
        } else {
            "HIGH"
        }
        .to_string();

        (score, label)
    }

fn snapshot(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let now_ts = chrono::Utc::now().timestamp();

        for t in self.trades.iter() {
            let pair = t.key().clone();
            let v = t.value();

            let has_whale = v.last_whale;
            let early = v
                .last_early
                .clone()
                .unwrap_or_else(|| "NONE".to_string());
            let alpha = v
                .last_alpha
                .clone()
                .unwrap_or_else(|| "NONE".to_string());
            let marked = self.signalled_pairs.get(&pair).is_some();

            if !has_whale && early == "NONE" && alpha == "NONE" && !marked {
                continue;
            }

            let buys = v.buy_volume;
            let sells = v.sell_volume;
            let flow_pct = v.last_flow_pct;
            let dir = if v.last_dir.is_empty() {
                "NONE".to_string()
            } else {
                v.last_dir.clone()
            };

            let c = self.candles.get(&pair);
            let (o, h, l, cl, pct) = if let Some(c) = c {
                (
                    c.open.unwrap_or(0.0),
                    c.high.unwrap_or(0.0),
                    c.low.unwrap_or(0.0),
                    c.close.unwrap_or(0.0),
                    c.pct_change.unwrap_or(0.0),
                )
            } else {
                (0.0, 0.0, 0.0, 0.0, 0.0)
            };

            let whale_side = v
                .last_whale_side
                .clone()
                .unwrap_or_else(|| "-".to_string());
            let whale_volume = v.last_whale_volume.unwrap_or(0.0);
            let whale_notional = v.last_whale_notional.unwrap_or(0.0);

            let rating = v
                .last_rating
                .clone()
                .unwrap_or_else(|| "NONE".to_string());

            let whale_pred_score = v.whale_pred_score;
            let whale_pred_label = v
                .whale_pred_label
                .clone()
                .unwrap_or_else(|| "NONE".to_string());

                        let (reliability_score, reliability_label) = Self::compute_reliability(&v, now_ts);

            rows.push(Row {
                pair,
                price: cl,
                pct,
                whale: has_whale,
                whale_side,
                whale_volume,
                whale_notional,
                flow_pct,
                dir,
                early,
                alpha,
                pump_score: v.last_pump_score,
                pump_label: v
                    .last_pump_signal
                    .clone()
                    .unwrap_or_else(|| "NONE".to_string()),
                trades: v.trade_count,
                buys,
                sells,
                o,
                h,
                l,
                c: cl,
                score: v.last_score,
                rating,
                whale_pred_score,
                whale_pred_label,
                reliability_score,
                reliability_label,
            });
        }

        rows.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        rows
    }

    fn signals_snapshot(&self) -> Vec<SignalEvent> {
        let buf = self.signals.lock().unwrap();
        let mut v: Vec<SignalEvent> = buf.iter().cloned().collect();
        v.sort_by(|a, b| b.ts.cmp(&a.ts));
        v
    }

    fn heatmap_snapshot(&self) -> Vec<HeatmapPoint> {
        self.snapshot()
            .into_iter()
            .map(|r| HeatmapPoint {
                pair: r.pair.clone(),
                flow_pct: r.flow_pct,
                pump_score: r.pump_score.max(0.0).min(10.0),
                ts: self
                    .trades
                    .get(&r.pair)
                    .map(|t| t.last_update_ts)
                    .unwrap_or(0),
            })
            .collect()
    }

    fn backtest_snapshot(&self) -> Vec<BacktestResult> {
        let sigs = self.signals.lock().unwrap();
        let mut groups: HashMap<(String, String), Vec<(i64, f64)>> = HashMap::new();

        for ev in sigs.iter() {
            if !ev.evaluated {
                continue;
            }
            if let Some(r) = ev.ret_5m {
                let key = (ev.signal_type.clone(), ev.direction.clone());
                groups.entry(key).or_default().push((ev.ts, r));
            }
        }

        let mut out = Vec::new();

        for ((signal_type, direction), mut trades) in groups {
            trades.sort_by_key(|(ts, _)| *ts);
            let n = trades.len();
            if n == 0 {
                continue;
            }

            let mut equity_curve = Vec::with_capacity(n);
            let mut cum = 0.0_f64;
            let mut peak = 0.0_f64;
            let mut max_dd = 0.0_f64;

            let mut wins = 0usize;
            let mut losses = 0usize;
            let mut win_sum = 0.0_f64;
            let mut loss_sum = 0.0_f64;
            let mut pnl_sum = 0.0_f64;

            let mut best_trade = f64::MIN;
            let mut worst_trade = f64::MAX;

            let mut losing_streak = 0usize;
            let mut max_losing_streak = 0usize;

            for (_ts, r) in trades.iter() {
                let r = *r;

                pnl_sum += r;
                cum += r;
                equity_curve.push(cum);

                if cum > peak {
                    peak = cum;
                }
                let dd = peak - cum;
                if dd > max_dd {
                    max_dd = dd;
                }

                if r > best_trade {
                    best_trade = r;
                }
                if r < worst_trade {
                    worst_trade = r;
                }

                if r > 0.0 {
                    wins += 1;
                    win_sum += r;
                    losing_streak = 0;
                } else {
                    losses += 1;
                    loss_sum += r;
                    losing_streak += 1;
                    if losing_streak > max_losing_streak {
                        max_losing_streak = losing_streak;
                    }
                }
            }

            let winrate = (wins as f64 / n as f64) * 100.0;
            let avg_win = if wins > 0 {
                win_sum / wins as f64
            } else {
                0.0
            };
            let avg_loss = if losses > 0 {
                loss_sum / losses as f64
            } else {
                0.0
            };
            let expectancy = pnl_sum / n as f64;

            out.push(BacktestResult {
                signal_type,
                direction,
                total_trades: n,
                winrate,
                avg_win,
                avg_loss,
                expectancy,
                pnl_sum,
                max_drawdown: max_dd,
                best_trade: if best_trade == f64::MIN {
                    0.0
                } else {
                    best_trade
                },
                worst_trade: if worst_trade == f64::MAX {
                    0.0
                } else {
                    worst_trade
                },
                max_losing_streak,
                equity_curve,
            });
        }

        out.sort_by(|a, b| {
            b.expectancy
                .partial_cmp(&a.expectancy)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        out
    }

    // ---------- TRADE ADVICE SNAPSHOT ----------
    fn trade_advice_snapshot(&self) -> TradeAdviceResponse {
        // Gebruik de huidige snapshot, gefilterd op de top van de scores
        let rows = self.snapshot();

        let mut advice_rows: Vec<TradeAdviceRow> = Vec::new();

        let mut total_entry = 0.0_f64;
        let mut total_exit_5 = 0.0_f64;
        let mut total_exit_10 = 0.0_f64;
        let mut total_exit_15 = 0.0_f64;
        let mut total_exit_20 = 0.0_f64;

        // Neem b.v. top 10 rows als advies-universum
        for r in rows.iter().take(10) {
            if r.price <= 0.0 {
                continue;
            }

            // Flow-score benadering zoals in handle_trade, maar alleen op basis van flow_pct
            let mut flow_score_for_entry = 0.0;
            if r.flow_pct > 75.0 {
                flow_score_for_entry = 3.0;
            } else if r.flow_pct > 65.0 {
                flow_score_for_entry = 2.0;
            } else if r.flow_pct > 55.0 {
                flow_score_for_entry = 1.0;
            }

            let pump_score = r.pump_score.max(0.0);
            let whale_pred_score = r.whale_pred_score.max(0.0);

            // OPTIE 3 — Entry = weighted model van totale score
            // entry = current_price - (
            //      0.001 * flow_score
            //    + 0.001 * pump_score
            //    + 0.002 * whale_pred_score
            // )
            let mut entry_price = r.price
                - (0.001 * flow_score_for_entry
                    + 0.001 * pump_score
                    + 0.002 * whale_pred_score);

            if entry_price <= 0.0 {
                entry_price = r.price * 0.995; // fallback
            }

            let exit_5 = entry_price * 1.05;
            let exit_10 = entry_price * 1.10;
            let exit_15 = entry_price * 1.15;
            let exit_20 = entry_price * 1.20;

            total_entry += entry_price;
            total_exit_5 += exit_5;
            total_exit_10 += exit_10;
            total_exit_15 += exit_15;
            total_exit_20 += exit_20;

            advice_rows.push(TradeAdviceRow {
                pair: r.pair.clone(),
                price: r.price,
                entry_price,
                exit_5,
                exit_10,
                exit_15,
                exit_20,
            });
        }

        let equity = TradeAdviceEquity {
            equity_5: total_exit_5,
            equity_10: total_exit_10,
            equity_15: total_exit_15,
            equity_20: total_exit_20,
        };

        TradeAdviceResponse {
            rows: advice_rows,
            equity,
        }
    }


    fn maybe_open_position_from_signal(&self, ev: &SignalEvent) {
        if ev.direction != "BUY" {
            return;
        }
        if ev.rating != "ALPHA BUY" && ev.rating != "STRONG BUY" {
            return;
        }
        if ev.price <= 0.0 {
            return;
        }
        let mut trader = self.trader.lock().unwrap();
        if trader.positions.contains_key(&ev.pair) {
            return;
        }
        if trader.positions.len() >= trader.max_positions {
            return;
        }
        let notional = trader.base_notional;
        let size = notional / ev.price;
        let sl = ev.price * (1.0 - VIRTUAL_SL_PCT);
        let tp = ev.price * (1.0 + VIRTUAL_TP_PCT);
        let pos = Position {
            pair: ev.pair.clone(),
            entry_price: ev.price,
            size,
            open_ts: ev.ts,
            stop_loss: sl,
            take_profit: tp,
        };
        trader.positions.insert(ev.pair.clone(), pos);
        println!(
            "[TRADER] OPEN LONG {} at {:.5} size {:.5} SL={:.5} TP={:.5} (rating={})",
            ev.pair, ev.price, size, sl, tp, ev.rating
        );
    }

    fn handle_price_for_trader(&self, pair: &str, price: f64, _ts: i64) {
        let mut trader = self.trader.lock().unwrap();
        if let Some(pos) = trader.positions.get(pair).cloned() {
            let hit_sl = price <= pos.stop_loss;
            let hit_tp = price >= pos.take_profit;
            if hit_sl || hit_tp {
                let pnl = (price - pos.entry_price) * pos.size;
                trader.balance += pnl;
                trader.realized_pnl += pnl;
                trader.positions.remove(pair);
                println!(
                    "[TRADER] CLOSE LONG {} at {:.5} (entry {:.5}, size {:.5}) PnL={:.4}{}",
                    pair,
                    price,
                    pos.entry_price,
                    pos.size,
                    pnl,
                    if hit_tp { " (TP)" } else { " (SL)" }
                );
            }
        }
    }

    fn positions_snapshot(&self) -> PositionsResponse {
        let trader = self.trader.lock().unwrap();
        let mut unrealized = 0.0_f64;
        let mut list = Vec::new();
        for (pair, pos) in trader.positions.iter() {
            let current_price = self
                .candles
                .get(pair)
                .and_then(|c| c.close)
                .unwrap_or(pos.entry_price);
            let pnl = (current_price - pos.entry_price) * pos.size;
            let pnl_pct = if pos.entry_price > 0.0 {
                (current_price - pos.entry_price) / pos.entry_price * 100.0
            } else {
                0.0
            };
            unrealized += pnl;
            list.push(PositionView {
                pair: pair.clone(),
                entry_price: pos.entry_price,
                size: pos.size,
                open_ts: pos.open_ts,
                stop_loss: pos.stop_loss,
                take_profit: pos.take_profit,
                current_price,
                pnl_abs: pnl,
                pnl_pct,
            });
        }
        PositionsResponse {
            balance: trader.balance + unrealized,
            realized_pnl: trader.realized_pnl,
            unrealized_pnl: unrealized,
            positions: list,
        }
    }

    fn build_analysis(row: &Row) -> String {
        let mut parts: Vec<String> = Vec::new();

        if row.pct > 15.0 {
            parts.push("Zeer sterke stijging (>15%).".to_string());
        } else if row.pct > 5.0 {
            parts.push("Gezonde opwaartse move (>5%).".to_string());
        } else if row.pct < -5.0 {
            parts.push("Duidelijke neerwaartse move (<-5%).".to_string());
        } else if row.pct < -2.0 {
            parts.push("Lichte downtrend.".to_string());
        } else if row.pct > 0.0 {
            parts.push("Lichte uptrend.".to_string());
        }

        if row.flow_pct > 75.0 && row.dir == "BUY" {
            parts.push("Kopers domineren (>75% buy-flow).".to_string());
        } else if row.flow_pct > 60.0 && row.dir == "BUY" {
            parts.push("Kopers domineren (>60% buy-flow).".to_string());
        } else if row.flow_pct > 60.0 && row.dir == "SELL" {
            parts.push("Verkopers domineren (>60% sell-flow).".to_string());
        }

        if row.alpha == "BUY" {
            parts.push("Alpha BUY: sterke combinatie van trend + anomaly.".to_string());
        }

        if row.whale_pred_label == "HIGH" {
            parts.push("Hoge kans op aankomende whale-activiteit.".to_string());
        } else if row.whale_pred_label == "MEDIUM" {
            parts.push("Verhoogde kans op whale-accumulatie.".to_string());
        }

        if parts.is_empty() {
            "Lichte downtrend.".to_string()
        } else {
            parts.join(" ")
        }
    }

    fn top10_snapshot(&self) -> Top10Response {
        let rows = self.snapshot();

        // stijgers
        let mut risers: Vec<TopRow> = rows
            .iter()
            .filter(|r| r.dir == "BUY" && r.pct > 0.0)
            .map(|r| TopRow {
                ts: self
                    .trades
                    .get(&r.pair)
                    .map(|t| t.last_update_ts)
                    .unwrap_or(0),
                pair: r.pair.clone(),
                price: r.price,
                pct: r.pct,
                flow_pct: r.flow_pct,
                dir: r.dir.clone(),
                early: r.early.clone(),
                alpha: r.alpha.clone(),
                pump_score: r.pump_score,
                pump_label: r.pump_label.clone(),
                whale: r.whale,
                whale_side: r.whale_side.clone(),
                whale_volume: r.whale_volume,
                whale_notional: r.whale_notional,
                total_score: r.score,
                analysis: Self::build_analysis(r),
                whale_pred_score: r.whale_pred_score,
                whale_pred_label: r.whale_pred_label.clone(),
                reliability_score: r.reliability_score,
                reliability_label: r.reliability_label.clone(),
            })
            .collect();

        // Best 3: hoogste (score + pump * 1.5 + whale_pred_score * 1.0)
        let mut best3 = risers.clone();
        best3.sort_by(|a, b| {
            let sa = a.total_score + a.pump_score * 1.5 + a.whale_pred_score * 1.0;
            let sb = b.total_score + b.pump_score * 1.5 + b.whale_pred_score * 1.0;
            sb.partial_cmp(&sa).unwrap()
        });
        if best3.len() > 3 {
            best3.truncate(3);
        }

        // Top 10 stijgers
        risers.sort_by(|a, b| {
            let sa = a.total_score + a.pump_score * 1.5 + a.whale_pred_score * 1.0;
            let sb = b.total_score + b.pump_score * 1.5 + b.whale_pred_score * 1.0;
            sb.partial_cmp(&sa).unwrap()
        });
        if risers.len() > 10 {
            risers.truncate(10);
        }

        // dalers
        let mut fallers: Vec<TopRow> = rows
            .iter()
            .filter(|r| r.dir == "SELL" && r.pct < 0.0)
            .map(|r| {
                let pct_down = (-r.pct).max(0.0);
                let flow_sell = if r.flow_pct > 50.0 {
                    r.flow_pct - 50.0
                } else {
                    0.0
                };
                let total_score = pct_down * 0.5 + flow_sell * 0.1;

                TopRow {
                    ts: self
                        .trades
                        .get(&r.pair)
                        .map(|t| t.last_update_ts)
                        .unwrap_or(0),
                    pair: r.pair.clone(),
                    price: r.price,
                    pct: r.pct,
                    flow_pct: r.flow_pct,
                    dir: r.dir.clone(),
                    early: r.early.clone(),
                    alpha: r.alpha.clone(),
                    pump_score: r.pump_score,
                    pump_label: r.pump_label.clone(),
                    whale: r.whale,
                    whale_side: r.whale_side.clone(),
                    whale_volume: r.whale_volume,
                    whale_notional: r.whale_notional,
                    total_score,
                    analysis: Self::build_analysis(r),
                    whale_pred_score: r.whale_pred_score,
                    whale_pred_label: r.whale_pred_label.clone(),
	            reliability_score: r.reliability_score,
		    reliability_label: r.reliability_label.clone(),

                }
            })
            .collect();

        fallers.sort_by(|a, b| b.total_score.partial_cmp(&a.total_score).unwrap());
        if fallers.len() > 10 {
            fallers.truncate(10);
        }

        Top10Response {
            best3,
            risers,
            fallers,
        }
    }
}

// ============================================================================
// HOOFDSTUK 8 – NORMALISATIE (ASSETS & PAIRS)
// ============================================================================


fn normalize_asset(sym: &str) -> String {
    match sym {
        "XBT" | "XXBT" => "BTC".to_string(),
        "XETH" => "ETH".to_string(),
        "XXRP" => "XRP".to_string(),
        "XDG" => "DOGE".to_string(),
        s => s.to_string(),
    }
}

fn normalize_pair(wsname: &str) -> String {
    let parts: Vec<&str> = wsname.split('/').collect();
    if parts.len() != 2 {
        return wsname.to_string();
    }
    let base = normalize_asset(parts[0]);
    let quote = normalize_asset(parts[1]);
    format!("{}/{}", base, quote)
}

// ============================================================================
// HOOFDSTUK 9 – FRONTEND (HTML DASHBOARD)
// ============================================================================


const DASHBOARD_HTML: &str = r####"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>WhaleRadar</title>
<style>
body { margin:0; background:#1e1e1e; color:#ddd; font-family:Arial; }
header { background:#111; padding:12px; display:flex; flex-direction:column; gap:8px; }
.header-top { display:flex; align-items:center; gap:12px; }
header h1 { margin:0; }
#search { flex:1; padding:6px; background:#222; border:1px solid #444; color:#fff; }
#tabs { display:flex; gap:6px; }
.tab-btn {
  padding:6px 10px;
  border:none;
  background:#222;
  color:#ccc;
  cursor:pointer;
  font-size:12px;
}
.tab-btn.active { background:#444; color:#fff; }
table { width:100%; border-collapse:collapse; margin-top:10px; font-size:12px; }
th { background:#222; padding:6px; border-bottom:1px solid #333; text-align:left; }
td { padding:6px; border-bottom:1px solid #333; }
tr:nth-child(even){ background:#252525; }
.pos { color:#4caf50; }
.neg { color:#f44336; }
.whale { color:#ffeb3b; font-weight:bold; }
.early { color:#ffc107; font-weight:bold; }
.alpha_buy { color:#00e676; font-weight:bold; }
.alpha_sell { color:#ff1744; }
.signal_type { font-weight:bold; }
.signal_type_EARLY { color:#ffc107; }
.signal_type_ALPHA { color:#00e676; }
.signal_type_WHALE { color:#ffeb3b; }
.signal_type_ANOM { color:#ff9800; }
.signal_type_EARLY_PUMP { color:#00bcd4; }
.signal_type_MEGA_PUMP { color:#ff4081; }
.signal_type_WH_PRED { color:#00bcd4; }
.signal_dir_BUY { color:#00e676; }
.signal_dir_SELL { color:#ff1744; }
.flow-bar {
  display:inline-block;
  width:70px;
  height:6px;
  background:#333;
  border-radius:3px;
  overflow:hidden;
  margin-right:4px;
  vertical-align:middle;
}
.flow-fill {
  height:100%;
}
#guide {
  margin-top:10px;
  font-size:12px;
  line-height:1.5;
}
.pred_high { color:#ff4081; font-weight:bold; }
.pred_med { color:#ff9800; font-weight:bold; }
.pred_low { color:#00bcd4; }

.rel_high { color:#4caf50; font-weight:bold; }
.rel_med  { color:#cddc39; font-weight:bold; }
.rel_low  { color:#ff9800; font-weight:bold; }
.rel_bad  { color:#f44336; font-weight:bold; }
</style>
</head>
<body>
<header>
  <div class="header-top">
    <h1>WhaleRadar</h1>
    <input id="search" placeholder="Zoek coin (btc, eth, whale, alpha, anom)..." />
  </div>
  <div id="tabs">
    <button class="tab-btn active" data-tab="markets">Markets</button>
    <button class="tab-btn" data-tab="signals">Signals</button>
    <button class="tab-btn" data-tab="top10">Top 10</button>
    <button class="tab-btn" data-tab="trade_advice">Trade Advice</button>
    <button class="tab-btn" data-tab="backtest">Backtest</button>
    <button class="tab-btn" data-tab="heatmap">Heatmap</button>
    <button class="tab-btn" data-tab="guide">Guide</button>
  </div>
</header>
<main style="padding:0 8px 8px 8px;">
  <div id="view-markets">
    <table id="grid">
      <thead>
        <tr>
          <th>Pair</th><th>Price</th><th>%</th><th>Whale</th>
          <th>Flow</th><th>Dir</th><th>Early</th><th>Alpha</th><th>Pump</th>
          <th>WhPred</th><th>Rel</th>
          <th>Trades</th><th>Buys</th><th>Sells</th>
          <th>O</th><th>H</th><th>L</th><th>C</th>
          <th>Visual</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
  </div>

  <div id="view-signals" style="display:none;">
    <table id="signals">
      <thead>
        <tr>
          <th>Time (ts)</th><th>Pair</th><th>Type</th><th>Dir</th>
          <th>Strength</th><th>Flow</th><th>%</th>
          <th>Whale</th><th>Vol</th><th>Notional</th><th>Price</th><th>Pump</th>
          <th>Visual</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
  </div>

  <div id="view-top10" style="display:none;">
    <h2>🔥 Best 3 Right Now</h2>
    <table id="top3">
      <thead>
        <tr>
          <th>Time</th><th>Pair</th><th>Price</th><th>%</th><th>Flow</th><th>Dir</th>
          <th>Early</th><th>Alpha</th><th>Whale</th><th>Total score</th><th>Pump</th>
          <th>WhPred</th>
          <th>Visual</th><th>Analyse</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>

    <h2>Top 10 Stijgers (strong buy)</h2>
    <table id="top10-up">
      <thead>
        <tr>
          <th>Time</th><th>Pair</th><th>Price</th><th>%</th><th>Flow</th><th>Dir</th>
          <th>Early</th><th>Alpha</th><th>Whale</th><th>Total score</th><th>Pump</th>
          <th>WhPred</th>
          <th>Visual</th><th>Analyse</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>

    <h2>Top 10 Dalers (strong sell)</h2>
    <table id="top10-down">
      <thead>
        <tr>
          <th>Time</th><th>Pair</th><th>Price</th><th>%</th><th>Flow</th><th>Dir</th>
          <th>Early</th><th>Alpha</th><th>Whale</th><th>Total score</th><th>Pump</th>
          <th>WhPred</th>
          <th>Visual</th><th>Analyse</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
  </div>

  <div id="view-trade_advice" style="display:none;">
    <h2>Trade Advice</h2>
    <p style="font-size:12px;">
      Entry prijs wordt live bepaald op basis van flow, pump en whale prediction (optie 3).
      Scenario's tonen mogelijke exits bij +5%, +10%, +15%, +20% vanaf entry.
    </p>
    <table id="trade-advice-table">
      <thead>
        <tr>
          <th>Munt</th>
          <th>Koers</th>
          <th>Entry</th>
          <th>Exit +5%</th>
          <th>Exit +10%</th>
          <th>Exit +15%</th>
          <th>Exit +20%</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>

    <h3>Equity per scenario (1 unit per entry)</h3>
    <table>
      <thead>
        <tr>
          <th>Equity bij +5%</th>
          <th>Equity bij +10%</th>
          <th>Equity bij +15%</th>
          <th>Equity bij +20%</th>
        </tr>
      </thead>
      <tbody id="trade-advice-equity">
      </tbody>
    </table>
  </div>

  <div id="view-backtest" style="display:none;">
    <h2>Backtest per signaaltype</h2>
    <p style="font-size:12px;">
      Gebaseerd op afgeronde signals (ongeveer 5 minuten na het signaal).
      Alle waarden zijn % prijsverandering per trade.
    </p>

    <table id="backtest-table">
      <thead>
        <tr>
          <th>Signaaltype</th>
          <th>Richting</th>
          <th>Trades</th>
          <th>Winrate</th>
          <th>Avg win</th>
          <th>Avg loss</th>
          <th>Expectancy</th>
          <th>PnL som</th>
          <th>Max drawdown</th>
          <th>Best trade</th>
          <th>Worst trade</th>
          <th>Max losing streak</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>

    <h3>Equity curve (klik op een rij)</h3>
    <canvas id="backtest-equity" width="900" height="260"
            style="border:1px solid #333; background:#111;"></canvas>
    <div id="backtest-equity-label"
         style="margin-top:4px; font-size:12px; color:#aaa;">
      Klik op een rij om de equity curve van die strategie te zien.
    </div>
  </div>

  <div id="view-heatmap" style="display:none;">
    <h2>Heatmap: BUY-flow vs Pump-score</h2>
    <canvas id="heatCanvas" width="800" height="400" style="border:0;"></canvas>
    <div style="margin-top:8px; font-size:12px;">
      <span style="background:#ff4081; padding:2px 6px; border-radius:4px; margin-right:6px;">MEGA pump</span>
      <span style="background:#00bcd4; padding:2px 6px; border-radius:4px; margin-right:6px;">EARLY pump</span>
      <span style="background:#4caf50; padding:2px 6px; border-radius:4px;">Sterke buy-flow</span>
      <div style="margin-top:4px;">
        X-as: BUY-flow (%) &nbsp; | &nbsp; Y-as: Pump-score (0–10).<br/>
        Rechtsboven = sterkste pump-kandidaten.
      </div>
    </div>
  </div>

  <div id="view-guide" style="display:none;">
    <div id="guide">
      <h2>Kolommen uitleg</h2>
      <ul>
        <li><b>Flow</b>: percentage van volume dat BUY is in de laatste 60 seconden.</li>
        <li><b>Dir</b>: dominante richting van de recente flow (BUY / SELL / NEUTR).</li>
        <li><b>Early</b>: vroege accumulatie (BUY) op basis van total score.</li>
        <li><b>Alpha</b>: sterkste combinatie van trend, volume, whales en anomalies (alleen bij BUY).</li>
        <li><b>Pump</b>: gecombineerde score van korte en middellange termijn prijsimpuls + flow.</li>
        <li><b>WhPred</b>: kans op aankomende whale (LOW / MEDIUM / HIGH).</li>
        <li><b>Visual</b>: link naar de bijbehorende Kraken Pro grafiek.</li>
      </ul>
    </div>
  </div>
</main>
<script>
let activeTab = "markets";

let heatmapPoints = [];
let heatTooltip = null;

function ensureHeatTooltip() {
  if (heatTooltip) return;
  heatTooltip = document.createElement("div");
  heatTooltip.style.position = "fixed";
  heatTooltip.style.pointerEvents = "none";
  heatTooltip.style.background = "rgba(0,0,0,0.85)";
  heatTooltip.style.color = "#fff";
  heatTooltip.style.padding = "4px 6px";
  heatTooltip.style.borderRadius = "4px";
  heatTooltip.style.fontSize = "11px";
  heatTooltip.style.zIndex = "9999";
  heatTooltip.style.display = "none";
  document.body.appendChild(heatTooltip);
}

function switchTab(tab) {
  activeTab = tab;
  document.getElementById("view-markets").style.display =
    tab === "markets" ? "block" : "none";
  document.getElementById("view-signals").style.display =
    tab === "signals" ? "block" : "none";
  document.getElementById("view-top10").style.display =
    tab === "top10" ? "block" : "none";
  document.getElementById("view-trade_advice").style.display =
    tab === "trade_advice" ? "block" : "none";
  document.getElementById("view-backtest").style.display =
    tab === "backtest" ? "block" : "none";
  document.getElementById("view-heatmap").style.display =
    tab === "heatmap" ? "block" : "none";
  document.getElementById("view-guide").style.display =
    tab === "guide" ? "block" : "none";

  document.querySelectorAll(".tab-btn").forEach(btn => {
    btn.classList.toggle("active", btn.dataset.tab === tab);
  });

  if (tab === "heatmap") {
    loadHeatmap();
  } else if (tab === "backtest") {
    loadBacktest();
  } else if (tab === "trade_advice") {
    loadTradeAdvice();
  }
}

document.querySelectorAll(".tab-btn").forEach(btn => {
  btn.addEventListener("click", () => switchTab(btn.dataset.tab));
});

function buildVisualUrl(pair) {
  if (!pair.includes("/")) return null;
  let [base, quote] = pair.split("/");
  return "https://pro.kraken.com/app/trade/" +
         base.toLowerCase() + "-" + quote.toLowerCase();
}

async function loadMarkets() {
  let q = document.getElementById("search").value.toLowerCase();
  let res = await fetch("/api/stats");
  let data = await res.json();
  let tbody = document.querySelector("#grid tbody");
  tbody.innerHTML = "";

  let filtered = data.filter(r =>
    r.pair.toLowerCase().includes(q) ||
    (r.whale && q === "whale") ||
    (r.early && r.early.toLowerCase().includes(q)) ||
    (r.alpha && r.alpha.toLowerCase().includes(q))
  );

  for (let r of filtered) {
    let pctClass = r.pct > 0 ? "pos" : (r.pct < 0 ? "neg" : "");
    let whaleClass = r.whale ? "whale" : "";
    let whaleText = r.whale
      ? (r.whale_side.toUpperCase() + " " + r.whale_volume.toFixed(3) +
         " (" + (r.whale_notional/1000).toFixed(1) + "k)")
      : "No";

    let earlyClass = (r.early === "BUY" || r.early === "SELL") ? "early" : "";
    let alphaClass =
      r.alpha === "BUY" ? "alpha_buy" :
      r.alpha === "SELL" ? "alpha_sell" : "";

    let flowColor = r.dir === "BUY" ? "#4caf50" : "#f44336";

    let predClass = "";
    if (r.whale_pred_label === "HIGH") predClass = "pred_high";
    else if (r.whale_pred_label === "MEDIUM") predClass = "pred_med";
    else if (r.whale_pred_label === "LOW") predClass = "pred_low";

    let relClass = "";
    if (r.reliability_label === "HIGH") relClass = "rel_high";
    else if (r.reliability_label === "MEDIUM") relClass = "rel_med";
    else if (r.reliability_label === "LOW") relClass = "rel_low";
    else relClass = "rel_bad";

    let visualUrl = buildVisualUrl(r.pair);
    let visual = visualUrl ? `<a href="${visualUrl}" target="_blank">Visual</a>` : "-";

    let row = `<tr>
      <td>${r.pair}</td>
      <td>${r.price.toFixed(4)}</td>
      <td class="${pctClass}">${r.pct.toFixed(2)}%</td>
      <td class="${whaleClass}">${whaleText}</td>
      <td>
        <div class="flow-bar">
          <div class="flow-fill" style="width:${r.flow_pct.toFixed(0)}%;background:${flowColor};"></div>
        </div>
        ${r.flow_pct.toFixed(1)}%
      </td>
      <td>${r.dir}</td>
      <td class="${earlyClass}">${r.early}</td>
      <td class="${alphaClass}">${r.alpha}</td>
      <td style="color:${ r.pump_label === "MEGA_PUMP" ? "#ff4081" :
        r.pump_label === "EARLY_PUMP" ? "#00bcd4" :
        "#ccc"}">${r.pump_score.toFixed(1)}</td>
      <td class="${predClass}">${r.whale_pred_label} (${r.whale_pred_score.toFixed(1)})</td>
      <td class="${relClass}">${r.reliability_label} (${r.reliability_score.toFixed(0)})</td>
      <td>${r.trades}</td>
      <td>${r.buys.toFixed(4)}</td>
      <td>${r.sells.toFixed(4)}</td>
      <td>${r.o.toFixed(4)}</td>
      <td>${r.h.toFixed(4)}</td>
      <td>${r.l.toFixed(4)}</td>
      <td>${r.c.toFixed(4)}</td>
      <td>${visual}</td>
    </tr>`;

    tbody.innerHTML += row;
  }
}

async function loadSignals() {
  let res = await fetch("/api/signals");
  let data = await res.json();
  let tbody = document.querySelector("#signals tbody");
  tbody.innerHTML = "";

  for (let r of data) {
    let typeClass = "signal_type signal_type_" + r.signal_type;
    let dirClass = "signal_dir_" + r.direction;

    let whaleTxt = r.whale
      ? (r.whale_side.toUpperCase() + " " + r.volume.toFixed(3) +
         " (" + (r.notional/1000).toFixed(1) + "k)")
      : "No";

    let pumpText = (r.signal_type === "MEGA_PUMP" || r.signal_type === "EARLY_PUMP")
      ? r.strength.toFixed(1)
      : "-";
    let pumpColor = r.signal_type === "MEGA_PUMP" ? "#ff4081" :
      (r.signal_type === "EARLY_PUMP" ? "#00bcd4" : "#ccc");

    let visualUrl = buildVisualUrl(r.pair);
    let visual = visualUrl ? `<a href="${visualUrl}" target="_blank">Visual</a>` : "-";

    let row = `<tr>
      <td>${r.ts}</td>
      <td>${r.pair}</td>
      <td class="${typeClass}">${r.signal_type}</td>
      <td class="${dirClass}">${r.direction}</td>
      <td>${r.strength.toFixed(3)}</td>
      <td>${r.flow_pct.toFixed(1)}%</td>
      <td>${r.pct.toFixed(2)}%</td>
      <td>${whaleTxt}</td>
      <td>${r.volume.toFixed(4)}</td>
      <td>${(r.notional/1000).toFixed(1)}k</td>
      <td>${r.price.toFixed(4)}</td>
      <td style="color:${pumpColor}">${pumpText}</td>
      <td>${visual}</td>
    </tr>`;

    tbody.innerHTML += row;
  }
}

async function loadTop10() {
  let res = await fetch("/api/top10");
  let data = await res.json();

  let top3Body = document.querySelector("#top3 tbody");
  let upBody = document.querySelector("#top10-up tbody");
  let downBody = document.querySelector("#top10-down tbody");
  top3Body.innerHTML = "";
  upBody.innerHTML = "";
  downBody.innerHTML = "";

  function fmtTime(ts) {
    const d = new Date(ts * 1000);
    return d.toLocaleTimeString();
  }

  function renderRow(r) {
    let pctClass = r.pct > 0 ? "pos" : (r.pct < 0 ? "neg" : "");
    let flowColor = r.dir === "BUY" ? "#4caf50" : "#f44336";
    let whaleText = r.whale
      ? (r.whale_side.toUpperCase() + " " + r.whale_volume.toFixed(3) +
         " (" + (r.whale_notional/1000).toFixed(1) + "k)")
      : "No";
    let visualUrl = buildVisualUrl(r.pair);
    let visual = visualUrl ? `<a href="${visualUrl}" target="_blank">Visual</a>` : "-";

    let predClass = "";
    if (r.whale_pred_label === "HIGH") predClass = "pred_high";
    else if (r.whale_pred_label === "MEDIUM") predClass = "pred_med";
    else if (r.whale_pred_label === "LOW") predClass = "pred_low";

    return `<tr>
      <td>${fmtTime(r.ts)}</td>
      <td>${r.pair}</td>
      <td>${r.price.toFixed(4)}</td>
      <td class="${pctClass}">${r.pct.toFixed(2)}%</td>
      <td>
        <div class="flow-bar">
          <div class="flow-fill" style="width:${r.flow_pct.toFixed(0)}%;background:${flowColor};"></div>
        </div>
        ${r.flow_pct.toFixed(1)}%
      </td>
      <td>${r.dir}</td>
      <td>${r.early}</td>
      <td>${r.alpha}</td>
      <td>${whaleText}</td>
      <td>${r.total_score.toFixed(2)}</td>
      <td style="color:${ r.pump_label === "MEGA_PUMP" ? "#ff4081" :
        r.pump_label === "EARLY_PUMP" ? "#00bcd4" :
        "#ccc"}">${r.pump_score.toFixed(1)}</td>
      <td class="${predClass}">${r.whale_pred_label} (${r.whale_pred_score.toFixed(1)})</td>
      <td>${visual}</td>
      <td>${r.analysis}</td>
    </tr>`;
  }

  for (let r of data.best3) {
    top3Body.innerHTML += renderRow(r);
  }

  for (let r of data.risers) {
    upBody.innerHTML += renderRow(r);
  }

  for (let r of data.fallers) {
    downBody.innerHTML += renderRow(r);
  }
}

async function loadBacktest() {
  try {
    let res = await fetch("/api/backtest");
    let data = await res.json();
    let tbody = document.querySelector("#backtest-table tbody");
    if (!tbody) return;
    tbody.innerHTML = "";

    data.forEach((r, idx) => {
      let tr = document.createElement("tr");
      tr.innerHTML = `
        <td>${r.signal_type}</td>
        <td>${r.direction}</td>
        <td>${r.total_trades}</td>
        <td>${r.winrate.toFixed(1)}%</td>
        <td>${r.avg_win.toFixed(2)}%</td>
        <td>${r.avg_loss.toFixed(2)}%</td>
        <td>${r.expectancy.toFixed(2)}%</td>
        <td>${r.pnl_sum.toFixed(2)}%</td>
        <td>${r.max_drawdown.toFixed(2)}%</td>
        <td>${r.best_trade.toFixed(2)}%</td>
        <td>${r.worst_trade.toFixed(2)}%</td>
        <td>${r.max_losing_streak}</td>
      `;
      tr.addEventListener("click", () => {
        drawEquityCurve(r);
      });
      tbody.appendChild(tr);
    });

    if (data.length > 0) {
      drawEquityCurve(data[0]);
    } else {
      let canvas = document.getElementById("backtest-equity");
      let ctx = canvas.getContext("2d");
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      document.getElementById("backtest-equity-label").textContent =
        "Nog geen backtest-data (self-evaluator moet eerst enkele signals afronden).";
    }
  } catch (e) {
    console.error("Backtest load error:", e);
  }
}

function drawEquityCurve(result) {
  let canvas = document.getElementById("backtest-equity");
  if (!canvas) return;
  let ctx = canvas.getContext("2d");
  let eq = result.equity_curve || [];

  ctx.clearRect(0, 0, canvas.width, canvas.height);

  if (!eq.length) {
    document.getElementById("backtest-equity-label").textContent =
      `Geen equity curve beschikbaar voor ${result.signal_type} / ${result.direction}.`;
    return;
  }

  let minY = Math.min(...eq);
  let maxY = Math.max(...eq);
  if (minY === maxY) {
    minY -= 1;
    maxY += 1;
  }

  let padding = 20;
  let w = canvas.width - padding * 2;
  let h = canvas.height - padding * 2;

  ctx.strokeStyle = "#444";
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(padding, padding + h);
  ctx.lineTo(padding, padding);
  ctx.lineTo(padding + w, padding);
  ctx.stroke();

  ctx.strokeStyle = "#00e676";
  ctx.lineWidth = 2;
  ctx.beginPath();

  eq.forEach((yVal, i) => {
    let x = padding + (w * i) / Math.max(eq.length - 1, 1);
    let normY = (yVal - minY) / (maxY - minY);
    let y = padding + h - normY * h;

    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  });

  ctx.stroke();

  document.getElementById("backtest-equity-label").textContent =
    `${result.signal_type} / ${result.direction} | trades: ${result.total_trades} | ` +
    `expectancy: ${result.expectancy.toFixed(2)}% | max DD: ${result.max_drawdown.toFixed(2)}%`;
}

// ---------- TRADE ADVICE JS ----------

async function loadTradeAdvice() {
  try {
    let res = await fetch("/api/trade_advice");
    let data = await res.json();
    let tbody = document.querySelector("#trade-advice-table tbody");
    let eqBody = document.querySelector("#trade-advice-equity");
    if (!tbody || !eqBody) return;

    tbody.innerHTML = "";
    eqBody.innerHTML = "";

    for (let r of data.rows) {
      let tr = document.createElement("tr");
      tr.innerHTML = `
        <td>${r.pair}</td>
        <td>${r.price.toFixed(5)}</td>
        <td>${r.entry_price.toFixed(5)}</td>
        <td>${r.exit_5.toFixed(5)}</td>
        <td>${r.exit_10.toFixed(5)}</td>
        <td>${r.exit_15.toFixed(5)}</td>
        <td>${r.exit_20.toFixed(5)}</td>
      `;
      tbody.appendChild(tr);
    }

    let e = data.equity;
    if (e) {
      let tr = document.createElement("tr");
      tr.innerHTML = `
        <td>${e.equity_5.toFixed(5)}</td>
        <td>${e.equity_10.toFixed(5)}</td>
        <td>${e.equity_15.toFixed(5)}</td>
        <td>${e.equity_20.toFixed(5)}</td>
      `;
      eqBody.appendChild(tr);
    }
  } catch (err) {
    console.error("trade_advice error", err);
  }
}

function loadHeatmap() {
  fetch("/api/heatmap")
    .then(r => r.json())
    .then(data => {
      const canvas = document.getElementById("heatCanvas");
      if (!canvas) return;
      const ctx = canvas.getContext("2d");
      const w = canvas.width;
      const h = canvas.height;

      ctx.fillStyle = "#111";
      ctx.fillRect(0, 0, w, h);

      ctx.strokeStyle = "#666";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(40, h - 30);
      ctx.lineTo(w - 10, h - 30);
      ctx.moveTo(40, 10);
      ctx.lineTo(40, h - 30);
      ctx.stroke();

      ctx.fillStyle = "#ccc";
      ctx.font = "11px sans-serif";
      ctx.fillText("Flow %", w/2 - 20, h - 10);
      ctx.save();
      ctx.translate(10, h/2 + 20);
      ctx.rotate(-Math.PI/2);
      ctx.fillText("Pump-score", 0, 0);
      ctx.restore();

      const x_min = 0.0, x_max = 100.0;
      const y_min = 0.0, y_max = 10.0;

      function x_to_px(x) {
        let frac = (x - x_min) / (x_max - x_min);
        if (frac < 0) frac = 0;
        if (frac > 1) frac = 1;
        return 40 + frac * (w - 50);
      }
      function y_to_px(y) {
        let frac = (y - y_min) / (y_max - y_min);
        if (frac < 0) frac = 0;
        if (frac > 1) frac = 1;
        return (h - 30) - frac * (h - 50);
      }

      heatmapPoints = [];

      for (let p of data) {
        const x = x_to_px(p.flow_pct);
        const y = y_to_px(p.pump_score);

        let color = "#4caf50";
        if (p.pump_score >= 8.0 && p.flow_pct >= 80.0) {
          color = "#ff4081";
        } else if (p.pump_score >= 6.0 && p.flow_pct >= 70.0) {
          color = "#00bcd4";
        }

        ctx.beginPath();
        ctx.fillStyle = color;
        ctx.arc(x, y, 6, 0, Math.PI * 2);
        ctx.fill();

        heatmapPoints.push({
          x, y,
          pair: p.pair,
          flow: p.flow_pct,
          pump: p.pump_score,
          ts: p.ts,
          color
        });
      }
    })
    .catch(err => console.error("heatmap error", err));
}

window.addEventListener("load", () => {
  const canvas = document.getElementById("heatCanvas");
  if (!canvas) return;
  ensureHeatTooltip();

  canvas.addEventListener("mousemove", (ev) => {
    if (!heatmapPoints.length) return;
    const rect = canvas.getBoundingClientRect();
    const mx = ev.clientX - rect.left;
    const my = ev.clientY - rect.top;

    let closest = null;
    let closestDist = Infinity;
    for (let p of heatmapPoints) {
      const dx = p.x - mx;
      const dy = p.y - my;
      const d2 = dx*dx + dy*dy;
      if (d2 < closestDist) {
        closestDist = d2;
        closest = p;
      }
    }

    const R2 = 9*9;
    if (closest && closestDist <= R2) {
      heatTooltip.style.display = "block";
      if (!window.fmtTime) {
        window.fmtTime = function(ts) {
          const d = new Date(ts * 1000);
          const dd = String(d.getDate()).padStart(2,'0');
          const mm = String(d.getMonth()+1).padStart(2,'0');
          const hh = String(d.getHours()).padStart(2,'0');
          const mi = String(d.getMinutes()).padStart(2,'0');
          return `${dd}-${mm} ${hh}:${mi}`;
        }
      }
      heatTooltip.textContent =
        `${closest.pair} | ${fmtTime(closest.ts)} | Flow ${closest.flow.toFixed(1)}% | Pump ${closest.pump.toFixed(1)}`;
      heatTooltip.style.left = (ev.clientX + 12) + "px";
      heatTooltip.style.top  = (ev.clientY + 12) + "px";
    } else {
      heatTooltip.style.display = "none";
    }
  });

  canvas.addEventListener("mouseleave", () => {
    if (heatTooltip) heatTooltip.style.display = "none";
  });

  canvas.addEventListener("click", (ev) => {
    if (!heatmapPoints.length) return;
    const rect = canvas.getBoundingClientRect();
    const mx = ev.clientX - rect.left;
    const my = ev.clientY - rect.top;

    let closest = null;
    let closestDist = Infinity;
    for (let p of heatmapPoints) {
      const dx = p.x - mx;
      const dy = p.y - my;
      const d2 = dx*dx + dy*dy;
      if (d2 < closestDist) {
        closestDist = d2;
        closest = p;
      }
    }

    const R2 = 9*9;
    if (closest && closestDist <= R2) {
      const search = document.getElementById("search");
      if (search) search.value = closest.pair;
      switchTab("markets");
    }
  });
});

function tick() {
  if (activeTab === "markets") {
    loadMarkets();
  } else if (activeTab === "signals") {
    loadSignals();
  } else if (activeTab === "top10") {
    loadTop10();
  } else if (activeTab === "backtest") {
    loadBacktest();
  } else if (activeTab === "trade_advice") {
    loadTradeAdvice();
  }
}

setInterval(tick, 1000);
document.getElementById("search").addEventListener("input", () => {
  if (activeTab === "markets") loadMarkets();
});
tick();
</script>
</body>
</html>
"####;

// ============================================================================
// HOOFDSTUK 10 – WEBSOCKET WORKERS
// ============================================================================


async fn run_kraken_worker(
    engine: Engine,
    ws_pairs: Vec<String>,
    worker_id: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = "wss://ws.kraken.com";

    loop {
        println!(
            "WS{}: connecting to Kraken ({} pairs)...",
            worker_id,
            ws_pairs.len()
        );

        let connect_res = connect_async(url).await;
        let (ws, _) = match connect_res {
            Ok(v) => v,
            Err(e) => {
                eprintln!("WS{}: connect error {:?}, retry in 5s", worker_id, e);
                sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        println!("WS{}: connected", worker_id);

        let (mut write, mut read) = ws.split();

        let sub = serde_json::json!({
            "event": "subscribe",
            "pair": ws_pairs,
            "subscription": { "name": "trade" }
        });

        if let Err(e) = write.send(Message::Text(sub.to_string())).await {
            eprintln!(
                "WS{}: subscribe send error {:?}, reconnecting...",
                worker_id, e
            );
            sleep(Duration::from_secs(5)).await;
            continue;
        }

        println!(
            "WS{}: subscribed to {} pairs via WebSocket",
            worker_id,
            ws_pairs.len()
        );

        while let Some(msg_res) = read.next().await {
            let msg = match msg_res {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("WS{}: read error {:?}, reconnecting...", worker_id, e);
                    break;
                }
            };

            if let Ok(txt) = msg.to_text() {
                if txt.contains("\"event\"") {
                    continue;
                }
                if let Ok(val) = serde_json::from_str::<Value>(txt) {
                    if val.is_array() && val.as_array().unwrap().len() >= 4 {
                        let arr = val.as_array().unwrap();
                        let trades = arr[1].as_array().unwrap();
                        let pair_raw = arr[3].as_str().unwrap_or("UNKNOWN");
                        let pair = normalize_pair(pair_raw);

                        for t in trades {
                            let ta = t.as_array().unwrap();
                            let price: f64 =
                                ta[0].as_str().unwrap().parse().unwrap_or(0.0);
                            let vol: f64 =
                                ta[1].as_str().unwrap().parse().unwrap_or(0.0);
                            let ts: f64 =
                                ta[2].as_str().unwrap().parse().unwrap_or(0.0);
                            let side = ta[3].as_str().unwrap_or("b");

                            if price > 0.0 && vol > 0.0 {
                                engine.handle_trade(&pair, price, vol, side, ts);
                            }
                        }
                    }
                }
            }
        }

        eprintln!("WS{}: stream ended, reconnecting in 5s...", worker_id);
        sleep(Duration::from_secs(5)).await;
    }
}

async fn run_orderbook_worker(
    engine: Engine,
    ws_pairs: Vec<String>,
    worker_id: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = "wss://ws.kraken.com";

    loop {
        println!(
            "OB_WS{}: connecting to Kraken orderbook ({} pairs)...",
            worker_id,
            ws_pairs.len()
        );

        let connect_res = connect_async(url).await;
        let (ws, _) = match connect_res {
            Ok(v) => v,
            Err(e) => {
                eprintln!("OB_WS{}: connect error {:?}, retry in 5s", worker_id, e);
                sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        println!("OB_WS{}: connected", worker_id);

        let (mut write, mut read) = ws.split();

        // Subscribe to orderbook updates (depth 10)
        let sub = serde_json::json!({
            "event": "subscribe",
            "pair": ws_pairs,
            "subscription": { "name": "book", "depth": 10 }
        });

        if let Err(e) = write.send(Message::Text(sub.to_string())).await {
            eprintln!(
                "OB_WS{}: subscribe send error {:?}, reconnecting...",
                worker_id, e
            );
            sleep(Duration::from_secs(5)).await;
            continue;
        }

        println!(
            "OB_WS{}: subscribed to orderbook for {} pairs",
            worker_id,
            ws_pairs.len()
        );

        while let Some(msg_res) = read.next().await {
            let msg = match msg_res {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("OB_WS{}: read error {:?}, reconnecting...", worker_id, e);
                    break;
                }
            };

            if let Ok(txt) = msg.to_text() {
                if txt.contains("\"event\"") {
                    continue;
                }
                if let Ok(val) = serde_json::from_str::<Value>(txt) {
                    if val.is_array() {
                        let arr = val.as_array().unwrap();
                        if arr.len() >= 4 {
                            let pair_raw = arr[arr.len() - 1].as_str().unwrap_or("UNKNOWN");
                            let pair = normalize_pair(pair_raw);

                            // Parse orderbook data
                            if let Some(data) = arr.get(1).and_then(|v| v.as_object()) {
                                let ts_int = chrono::Utc::now().timestamp();
                                let mut bids: Vec<(f64, f64)> = Vec::new();
                                let mut asks: Vec<(f64, f64)> = Vec::new();

                                // Parse bids (either 'b' or 'bs')
                                if let Some(bid_arr) = data.get("b").or_else(|| data.get("bs")) {
                                    if let Some(bid_list) = bid_arr.as_array() {
                                        for item in bid_list {
                                            if let Some(bid) = item.as_array() {
                                                if bid.len() >= 2 {
                                                    let price: f64 = bid[0]
                                                        .as_str()
                                                        .unwrap_or("0")
                                                        .parse()
                                                        .unwrap_or(0.0);
                                                    let volume: f64 = bid[1]
                                                        .as_str()
                                                        .unwrap_or("0")
                                                        .parse()
                                                        .unwrap_or(0.0);
                                                    if price > 0.0 && volume > 0.0 {
                                                        bids.push((price, volume));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                // Parse asks (either 'a' or 'as')
                                if let Some(ask_arr) = data.get("a").or_else(|| data.get("as")) {
                                    if let Some(ask_list) = ask_arr.as_array() {
                                        for item in ask_list {
                                            if let Some(ask) = item.as_array() {
                                                if ask.len() >= 2 {
                                                    let price: f64 = ask[0]
                                                        .as_str()
                                                        .unwrap_or("0")
                                                        .parse()
                                                        .unwrap_or(0.0);
                                                    let volume: f64 = ask[1]
                                                        .as_str()
                                                        .unwrap_or("0")
                                                        .parse()
                                                        .unwrap_or(0.0);
                                                    if price > 0.0 && volume > 0.0 {
                                                        asks.push((price, volume));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                // Update orderbook in engine if we have data
                                if !bids.is_empty() || !asks.is_empty() {
                                    // Sort bids descending (highest first)
                                    bids.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
                                    // Sort asks ascending (lowest first)
                                    asks.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

                                    let ob_state = OrderbookState {
                                        bids,
                                        asks,
                                        timestamp: ts_int,
                                    };
                                    engine.orderbooks.insert(pair.clone(), ob_state);
                                }
                            }
                        }
                    }
                }
            }
        }

        eprintln!("OB_WS{}: stream ended, reconnecting in 5s...", worker_id);
        sleep(Duration::from_secs(5)).await;
    }
}

// ============================================================================
// HOOFDSTUK 11 – REST ANOMALY SCANNER
// ============================================================================


async fn run_anomaly_scanner(
    engine: Engine,
    kraken_keys: Vec<String>,
    key_to_norm: HashMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "Starting anomaly scanner over {} Kraken pairs (REST)...",
        kraken_keys.len()
    );

    loop {
        for chunk in kraken_keys.chunks(20) {
            let keys: Vec<String> = chunk.iter().cloned().collect();
            let joined = keys.join(",");
            let url =
                format!("https://api.kraken.com/0/public/Ticker?pair={}", joined);

            if let Ok(resp) = reqwest::get(&url).await {
                if let Ok(json) = resp.json::<Value>().await {
                    if let Some(obj) = json["result"].as_object() {
                        for (k, v) in obj.iter() {
                            let last_str = v["c"][0].as_str().unwrap_or("0");
                            let vol_str = v["v"][1].as_str().unwrap_or("0");
                            let open_str = v["o"].as_str().unwrap_or("0");

                            let last: f64 = last_str.parse().unwrap_or(0.0);
                            let vol24h: f64 = vol_str.parse().unwrap_or(0.0);
                            let open: f64 = open_str.parse().unwrap_or(0.0);

                            if last > 0.0 && open > 0.0 {
                                let ts_int = Utc::now().timestamp();
                                let norm = key_to_norm
                                    .get(k)
                                    .cloned()
                                    .unwrap_or_else(|| k.clone());
                                engine.handle_ticker(&norm, last, vol24h, open, ts_int);
                            }
                        }
                    }
                }
            }

            sleep(Duration::from_millis(500)).await;
        }

        sleep(Duration::from_secs(20)).await;
    }
}

// ============================================================================
// HOOFDSTUK 12 – SELF-EVALUATOR (ZELFLEREND)
// ============================================================================


async fn run_self_evaluator(engine: Engine) {
    loop {
        sleep(Duration::from_secs(60)).await;
        let now_ts = Utc::now().timestamp();

        let mut updated = false;
        {
            let mut weights = engine.weights.lock().unwrap();
            let mut sigs = engine.signals.lock().unwrap();

            for ev in sigs.iter_mut() {
                if ev.evaluated {
                    continue;
                }
                if now_ts - ev.ts < 300 {
                    continue;
                }
                if ev.rating == "NONE" {
                    ev.evaluated = true;
                    continue;
                }

                let current_price = engine
                    .candles
                    .get(&ev.pair)
                    .and_then(|c| c.close)
                    .unwrap_or(ev.price);

                let ret = (current_price - ev.price) / ev.price * 100.0;

                let success_strong = ret >= 2.0;
                let success_weak = ret >= 0.5 && ret < 2.0;
                let fail = ret <= -0.5;

                let strong_step_up = 1.02;
                let weak_step_up = 1.01;
                let step_down = 0.98;

                let adjust = |w: &mut f64, factor_score: f64| {
                    if factor_score <= 0.0 {
                        return;
                    }
                    if success_strong {
                        *w *= strong_step_up;
                    } else if success_weak {
                        *w *= weak_step_up;
                    } else if fail {
                        *w *= step_down;
                    }
                    if *w < 0.2 {
                        *w = 0.2;
                    }
                    if *w > 5.0 {
                        *w = 5.0;
                    }
                };

                adjust(&mut weights.flow_w, ev.flow_score);
                adjust(&mut weights.price_w, ev.price_score);
                adjust(&mut weights.whale_w, ev.whale_score);
                adjust(&mut weights.volume_w, ev.volume_score);
                adjust(&mut weights.anomaly_w, ev.anomaly_score);
                adjust(&mut weights.trend_w, ev.trend_score);

                // backtest-data invullen
                ev.ret_5m = Some(ret);
                ev.eval_horizon_sec = Some(now_ts - ev.ts);

                ev.evaluated = true;
                updated = true;
            }

            if updated {
                println!(
                    "Gewichten geüpdatet -> flow:{:.2} price:{:.2} whale:{:.2} vol:{:.2} anom:{:.2} trend:{:.2}",
                    weights.flow_w,
                    weights.price_w,
                    weights.whale_w,
                    weights.volume_w,
                    weights.anomaly_w,
                    weights.trend_w
                );
            }
        }
    }
}

// ============================================================================
// HOOFDSTUK 13 – CLEANUP & ONDERHOUD
// ============================================================================


async fn run_cleanup(engine: Engine) {
    loop {
        sleep(Duration::from_secs(600)).await;

        let now = Utc::now().timestamp();
        let cutoff_trades = now - 12 * 3600;
        let cutoff_candles = now - 24 * 3600;
        let cutoff_orderbooks = now - 60; // Remove orderbooks older than 1 minute

        engine.trades.retain(|_, v| v.last_update_ts >= cutoff_trades);

        let mut to_reset = Vec::new();
        for c in engine.candles.iter() {
            let last_ts = c.last_ts.unwrap_or(0);
            if last_ts < cutoff_candles {
                to_reset.push(c.key().clone());
            }
        }
        for k in to_reset {
            engine.candles.insert(k, CandleState::default());
        }

        // Cleanup old orderbooks
        engine.orderbooks.retain(|_, v| v.timestamp >= cutoff_orderbooks);

        println!("Cleanup: oude trades (>12u), candles (>24u) en orderbooks (>1m) opgeschoond.");
    }
}

// ============================================================================
// HOOFDSTUK 14 – HTTP SERVER & API
// ============================================================================


async fn run_http(engine: Engine) {
    let engine_filter = warp::any().map(move || engine.clone());

    let api_stats = warp::path!("api" / "stats")
        .and(engine_filter.clone())
        .map(|engine: Engine| warp::reply::json(&engine.snapshot()));

    let api_signals = warp::path!("api" / "signals")
        .and(engine_filter.clone())
        .map(|engine: Engine| warp::reply::json(&engine.signals_snapshot()));

    let api_top10 = warp::path!("api" / "top10")
        .and(engine_filter.clone())
        .map(|engine: Engine| warp::reply::json(&engine.top10_snapshot()));

    let api_heatmap = warp::path!("api" / "heatmap")
        .and(engine_filter.clone())
        .map(|engine: Engine| warp::reply::json(&engine.heatmap_snapshot()));

    let api_backtest = warp::path!("api" / "backtest")
        .and(engine_filter.clone())
        .map(|engine: Engine| warp::reply::json(&engine.backtest_snapshot()));

    let api_trade_advice = warp::path!("api" / "trade_advice")
        .and(engine_filter.clone())
        .map(|engine: Engine| warp::reply::json(&engine.trade_advice_snapshot()));

    let api_positions = warp::path!("api" / "positions")
        .and(engine_filter.clone())
        .map(|engine: Engine| warp::reply::json(&engine.positions_snapshot()));

    let index = warp::path::end().map(|| warp::reply::html(DASHBOARD_HTML));

    let routes = api_stats
        .or(api_signals)
        .or(api_top10)
        .or(api_heatmap)
        .or(api_backtest)
        .or(api_trade_advice)
        .or(api_positions)
        .or(index);

    let mut port: u16 = 8080;
    loop {
        let addr_str = format!("127.0.0.1:{}", port);

        match TcpListener::bind(&addr_str) {
            Ok(listener) => {
                drop(listener);
                println!("Dashboard: http://{}", addr_str);
                warp::serve(routes.clone())
                    .run(([127, 0, 0, 1], port))
                    .await;
                break;
            }
            Err(_) => {
                eprintln!("Port {} bezet, probeer volgende...", port);
                port += 1;
                if port > 8090 {
                    eprintln!(
                        "Geen vrije poort gevonden tussen 8080 en 8090, HTTP-server stopt."
                    );
                    break;
                }
            }
        }
    }
}

// ============================================================================
// HOOFDSTUK 15 – MAIN ENTRYPOINT
// ============================================================================


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Fetching Kraken markets...");
    let data: Value =
        reqwest::get("https://api.kraken.com/0/public/AssetPairs")
            .await?
            .json()
            .await?;

    let result = data["result"]
        .as_object()
        .expect("Invalid JSON from Kraken AssetPairs");
    println!("Kraken markets: {}", result.len());

    let mut kraken_keys: Vec<String> = Vec::new();
    let mut key_to_norm: HashMap<String, String> = HashMap::new();
    let mut ws_pairs: Vec<String> = Vec::new();

    for (k, v) in result.iter() {
        if let Some(wsname) = v["wsname"].as_str() {
            let norm = normalize_pair(wsname);
            if norm.ends_with("/EUR") {
                kraken_keys.push(k.clone());
                key_to_norm.insert(k.clone(), norm);
                ws_pairs.push(wsname.to_string());
            }
        }
    }

    kraken_keys.sort();
    if kraken_keys.len() > 500 {
        kraken_keys.truncate(500);
    }

    ws_pairs.sort();
    ws_pairs.dedup();
    let total_ws_pairs = ws_pairs.len();
    let chunks: Vec<Vec<String>> = ws_pairs.chunks(20).map(|c| c.to_vec()).collect();

    println!(
        "Using {} pairs for anomaly scanner (REST), {} EUR pairs via WebSocket trades ({} WS workers)",
        kraken_keys.len(),
        total_ws_pairs,
        chunks.len()
    );

    let engine = Engine::new();
    let engine_for_ws = engine.clone();

    // Clone chunks for orderbook workers
    let ob_chunks: Vec<Vec<String>> = ws_pairs.chunks(20).map(|c| c.to_vec()).collect();

    // Spawn trade WebSocket workers
    for (i, chunk) in chunks.into_iter().enumerate() {
        let e = engine_for_ws.clone();
        tokio::spawn(async move {
            if let Err(err) = run_kraken_worker(e, chunk, i).await {
                eprintln!("WS worker {} terminated with error: {:?}", i, err);
            }
        });
    }

    // Spawn orderbook WebSocket workers
    let engine_for_ob = engine.clone();
    for (i, chunk) in ob_chunks.into_iter().enumerate() {
        let e = engine_for_ob.clone();
        tokio::spawn(async move {
            if let Err(err) = run_orderbook_worker(e, chunk, i).await {
                eprintln!("OB worker {} terminated with error: {:?}", i, err);
            }
        });
    }

    let engine_http = engine.clone();
    let engine_anom = engine.clone();
    let engine_eval = engine.clone();
    let engine_cleanup = engine.clone();

    tokio::join!(
        async move { run_http(engine_http).await; },
        async move {
            let _ = run_anomaly_scanner(engine_anom, kraken_keys, key_to_norm).await;
        },
        async move { run_self_evaluator(engine_eval).await; },
        async move { run_cleanup(engine_cleanup).await; },
    );

    Ok(())
}