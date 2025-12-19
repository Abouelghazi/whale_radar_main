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
use lazy_static::lazy_static;
use reqwest;
use rss::Channel;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Cursor;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use tokio::time::{sleep, Duration};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use warp::Filter;

// ============================================================================
// LAZY STATIC INITIALIZATION
// ============================================================================

lazy_static! {
    static ref KEYWORD_MAP: HashMap<String, String> = {
        let json_str = include_str!("pair_keywords.json");
        serde_json::from_str(json_str).expect("Failed to parse pair_keywords.json: ensure valid JSON format with string key-value pairs")
    };
    
    // Pre-sorted keywords by length (descending) for efficient matching
    static ref SORTED_KEYWORDS: Vec<(String, String)> = {
        let mut keywords: Vec<(String, String)> = KEYWORD_MAP
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        keywords.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        keywords
    };
}

lazy_static! {
    static ref SENTIMENT_MAP: HashMap<String, Vec<(String, i32)>> = {
        match std::fs::read_to_string("sentiment_words.json") {
            Ok(content) => {
                match serde_json::from_str::<serde_json::Value>(&content) {
                    Ok(data) => {
                        let mut map = HashMap::new();
                        // Parse positive words
                        if let Some(pos_arr) = data["positive"].as_array() {
                            let pos_vec: Vec<(String, i32)> = pos_arr.iter().filter_map(|item| {
                                if let Some(arr) = item.as_array() {
                                    if arr.len() == 2 {
                                        let word = arr[0].as_str()?.to_string();
                                        let weight = arr[1].as_i64()? as i32;
                                        Some((word, weight))
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            }).collect();
                            map.insert("positive".to_string(), pos_vec);
                        }
                        // Parse negative words
                        if let Some(neg_arr) = data["negative"].as_array() {
                            let neg_vec: Vec<(String, i32)> = neg_arr.iter().filter_map(|item| {
                                if let Some(arr) = item.as_array() {
                                    if arr.len() == 2 {
                                        let word = arr[0].as_str()?.to_string();
                                        let weight = arr[1].as_i64()? as i32;
                                        Some((word, weight))
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            }).collect();
                            map.insert("negative".to_string(), neg_vec);
                        }
                        map
                    }
                    Err(e) => {
                        eprintln!("Failed to parse sentiment_words.json: {}", e);
                        HashMap::new()
                    }
                }
            }
            Err(_) => {
                eprintln!("Kon sentiment_words.json niet laden, gebruik lege lijsten");
                HashMap::new()
            }
        }
    };
}

// ============================================================================
// HOOFDSTUK 1 – CONFIGURATIE & CONSTANTES
// ============================================================================

// NIEUW: Uitgebreide Config struct voor alle aanpasbare instellingen
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppConfig {
    // 1. Signal Drempels
    pump_conf_threshold: f64,
    whale_pred_high_threshold: f64,
    early_buy_threshold: f64,
    alpha_buy_threshold: f64,
    strong_buy_threshold: f64,
    whale_min_notional: f64,
    anomaly_strength_threshold: f64,

    // 2. Score Gewichten
    flow_weight: f64,
    price_weight: f64,
    whale_weight: f64,
    volume_weight: f64,
    anomaly_weight: f64,
    trend_weight: f64,

    // 3. Paper Trading Instellingen
    initial_balance: f64,
    base_notional: f64,
    sl_pct: f64,
    tp_pct: f64,
    max_positions: usize,
    enable_trading: bool,

    // 4. Engine & Data Instellingen
    ws_workers_per_chunk: usize,
    rest_scan_interval_sec: u64,
    cleanup_interval_sec: u64,
    eval_horizon_sec: i64,
    max_history: usize,

    // 5. UI & Filter Instellingen
    default_dir_filter: String,
    include_stablecoins_default: bool,
    heatmap_min_radius: f64,
    heatmap_max_radius: f64,
    chart_refresh_rate_sec: f64,

    // 6. AI & Self-Learning Instellingen
    ai_success_threshold: f64,
    ai_adjustment_step_up: f64,
    ai_adjustment_step_down: f64,
    ai_max_weight: f64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            // Signal Drempels
            pump_conf_threshold: 0.7,
            whale_pred_high_threshold: 8.0,
            early_buy_threshold: 3.0,
            alpha_buy_threshold: 7.5,
            strong_buy_threshold: 5.0,
            whale_min_notional: 5000.0,
            anomaly_strength_threshold: 40.0,

            // Score Gewichten
            flow_weight: 2.2,
            price_weight: 0.7,
            whale_weight: 1.4,
            volume_weight: 1.3,
            anomaly_weight: 1.5,
            trend_weight: 1.1,

            // Paper Trading
            initial_balance: 10000.0,
            base_notional: 100.0,
            sl_pct: 0.02,
            tp_pct: 0.05,
            max_positions: 5,
            enable_trading: true,

            // Engine & Data
            ws_workers_per_chunk: 20,
            rest_scan_interval_sec: 20,
            cleanup_interval_sec: 600,
            eval_horizon_sec: 300,
            max_history: 400,

            // UI & Filter
            default_dir_filter: "ALL".to_string(),
            include_stablecoins_default: true,
            heatmap_min_radius: 4.0,
            heatmap_max_radius: 12.0,
            chart_refresh_rate_sec: 1.0,

            // AI & Self-Learning
            ai_success_threshold: 0.7,
            ai_adjustment_step_up: 1.02,
            ai_adjustment_step_down: 0.98,
            ai_max_weight: 5.0,
        }
    }
}

// Functies voor config laden/opslaan
const CONFIG_FILE: &str = "config.json";

async fn load_config() -> AppConfig {
    match tokio::fs::read_to_string(CONFIG_FILE).await {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => {
            let default = AppConfig::default();
            if let Ok(json) = serde_json::to_string_pretty(&default) {
                let _ = tokio::fs::write(CONFIG_FILE, json).await;
            }
            default
        }
    }
}

async fn save_config(config: &AppConfig) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string_pretty(config)?;
    tokio::fs::write(CONFIG_FILE, json).await?;
    Ok(())
}

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

    // NIEUW: Nieuws-sentiment integratie (stap 1)
    news_sentiment: f64,  // 0.0 = negatief, 1.0 = positief, default 0.5
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

    // news sentiment field
    news_sentiment: f64,
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

    reliability_score: f64,
    reliability_label: String,

    // toegevoegd: signal_type
    signal_type: String,
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
    reliability_score: f64,  // TOEGEVOEGD: REL voor heatmap
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


#[derive(Debug, Clone)]
struct Position {
    pair: String,
    entry_price: f64,
    size: f64,
    open_ts: i64,
    stop_loss: f64,
    take_profit: f64,
}

#[derive(Debug, Clone, Serialize)]
struct TradeRecord {
    pair: String,
    entry_price: f64,
    exit_price: f64,
    size: f64,
    pnl: f64,
    open_ts: i64,
    close_ts: i64,
    reason: String,
}

#[derive(Debug)]
struct TraderState {
    initial_balance: f64,
    balance: f64,
    base_notional: f64,
    max_positions: usize,
    positions: HashMap<String, Position>,
    realized_pnl: f64,
    trade_history: Vec<TradeRecord>,
    equity_curve: Vec<(i64, f64)>,
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
            trade_history: Vec::new(),
            equity_curve: vec![(chrono::Utc::now().timestamp(), VIRTUAL_INITIAL_BALANCE)],
        }
    }

    fn add_trade_record(&mut self, record: TradeRecord) {
        self.trade_history.push(record);
        if self.trade_history.len() > 1000 {
            self.trade_history.remove(0);
        }
        let now = chrono::Utc::now().timestamp();
        self.equity_curve.push((now, self.balance));
        if self.equity_curve.len() > 365 {
            self.equity_curve.remove(0);
        }
    }

    // NIEUW: Handmatige positie openen
    fn manual_open_position(&mut self, pair: &str, price: f64, sl_pct: f64, tp_pct: f64) -> bool {
        if self.positions.contains_key(pair) {
            return false; // Al een positie
        }
        if self.positions.len() >= self.max_positions {
            return false;
        }
        let notional = self.base_notional;
        let size = notional / price;
        let sl = price * (1.0 - sl_pct / 100.0);
        let tp = price * (1.0 + tp_pct / 100.0);
        let pos = Position {
            pair: pair.to_string(),
            entry_price: price,
            size,
            open_ts: chrono::Utc::now().timestamp(),
            stop_loss: sl,
            take_profit: tp,
        };
        self.positions.insert(pair.to_string(), pos);
        println!(
            "[MANUAL BUY] OPEN LONG {} at {:.5} size {:.5} SL={:.5} TP={:.5}",
            pair, price, size, sl, tp
        );
        true
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

#[derive(Debug, Clone, Serialize)]
struct PaperPortfolioResponse {
    balance: f64,
    realized_pnl: f64,
    unrealized_pnl: f64,
    positions: usize,
    total_trades: usize,
    winrate: f64,
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
    news_sentiment: Arc<DashMap<String, (f64, i64, String)>>,
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
            news_sentiment: Arc::new(DashMap::new()),
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

    // NIEUW: update_sentiment functie voor nieuws-integratie (stap 1)
    fn update_sentiment(&self, pair: &str, sentiment: f64, title: &str) {
        self.news_sentiment.insert(pair.to_string(), (sentiment, Utc::now().timestamp(), title.to_string()));
        // Optioneel: Boost scores in trades als de pair bestaat
        if let Some(mut ts) = self.trades.get_mut(pair) {
            ts.news_sentiment = sentiment;
            ts.last_update_ts = Utc::now().timestamp();  // FIX: Update timestamp zodat UI echte data toont
            // Boost scores: Bij positief sentiment, verhoog anomaly_score
            if sentiment > 0.7 {
                ts.last_score *= 1.1;  // 10% boost
            } else if sentiment < 0.3 {
                ts.last_score *= 0.95;  // Lichte penalty voor negatief
            }
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

        // Orderbook integration for whale prediction
        if let Some(ob) = self.orderbooks.get(pair) {
            let age = ts_int.saturating_sub(ob.timestamp);
            if age >= 0 && age <= 10 {
                let bid_volume: f64 = ob.bids.iter().take(10).map(|(_, v)| v).sum();
                let ask_volume: f64 = ob.asks.iter().take(10).map(|(_, v)| v).sum();
                let total_volume = bid_volume + ask_volume;
                if total_volume > 0.0 {
                    let bid_ratio = bid_volume / total_volume;
                    if bid_ratio > 0.65 {
                        whale_pred_score += (bid_ratio - 0.65) * 2.0;
                    }
                }
            }
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
        let _vol_60: f64 = 0.0; // Fix unused variable
        for (_ts, _v) in t.recent_buys.iter().chain(t.recent_sells.iter()) {
            if *_ts >= cutoff_60 {
                recent_trades_60 += 1;
                // _vol_60 += *v; // Unused, so commented
            }
        }

        let mut vol_300: f64 = 0.0;
        for (_ts, v) in t.recent_buys_5m.iter().chain(t.recent_sells_5m.iter()) {
            if *_ts >= cutoff_300 {
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
        for (_ts, v) in t.recent_buys.iter() {
            if *_ts >= cutoff_60 {
                buys_60 += *v;
            }
        }
        for (_ts, v) in t.recent_sells.iter() {
            if *_ts >= cutoff_60 {
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
                pair: pair.clone(),
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
                news_sentiment: self.news_sentiment.get(&pair).map(|v| v.0).unwrap_or(0.5),
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
                reliability_score: r.reliability_score,  // TOEGEVOEGD: REL voor heatmap
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

            let best_trade = f64::MIN; // Fix mut
            let worst_trade = f64::MAX;

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
                let record = TradeRecord {
                    pair: pair.to_string(),
                    entry_price: pos.entry_price,
                    exit_price: price,
                    size: pos.size,
                    pnl,
                    open_ts: pos.open_ts,
                    close_ts: chrono::Utc::now().timestamp(),
                    reason: if hit_tp { "TP" } else { "SL" }.to_string(),
                };
                trader.add_trade_record(record);
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

        // Prijsbeweging
        if row.pct > 5.0 {
            parts.push(format!("Prijs is gestegen met {:.1}%.", row.pct));
        } else if row.pct > 1.0 {
            parts.push(format!("Lichte prijsstijging van {:.1}%.", row.pct));
        } else if row.pct < -1.0 {
            parts.push(format!("Prijs is gedaald met {:.1}%.", row.pct.abs()));
        } else {
            parts.push("Prijs beweegt zijwaarts.".to_string());
        }

        // Flow en richting
        if row.flow_pct > 70.0 && row.dir == "BUY" {
            parts.push(format!("Sterke koopdruk: {:.0}% buy-flow.", row.flow_pct));
        } else if row.flow_pct > 60.0 && row.dir == "BUY" {
            parts.push(format!("Matige koopdruk: {:.0}% buy-flow.", row.flow_pct));
        } else if row.flow_pct > 60.0 && row.dir == "SELL" {
            parts.push(format!("Verkoopdruk: {:.0}% sell-flow.", row.flow_pct));
        } else {
            parts.push("Neutrale markt flow.".to_string());
        }

        // Whale activiteit
        if row.whale {
            let whale_vol = row.whale_volume;
            let whale_not = row.whale_notional / 1000.0;
            parts.push(format!("Whale-trade gedetecteerd: {:.2} eenheden, €{:.0}k notional.", whale_vol, whale_not));
        }

        // Pump score
        if row.pump_score > 5.0 {
            parts.push(format!("Pump-score van {:.1} duidt op mogelijke accumulatie.", row.pump_score));
        } else if row.pump_score > 2.0 {
            parts.push(format!("Matige pump-score van {:.1}.", row.pump_score));
        }

        // Whale prediction
        if row.whale_pred_label == "HIGH" {
            parts.push(format!("Hoge kans op whale-activiteit (score {:.1}).", row.whale_pred_score));
        } else if row.whale_pred_label == "MEDIUM" {
            parts.push(format!("Matige kans op whales (score {:.1}).", row.whale_pred_score));
        }

        // Reliability
        if row.reliability_label == "HIGH" {
            parts.push(format!("Betrouwbaarheid hoog ({:.0}).", row.reliability_score));
        } else if row.reliability_label == "LOW" {
            parts.push(format!("Betrouwbaarheid laag ({:.0}) - let op.", row.reliability_score));
        }

        // Alpha signalen
        if row.alpha == "BUY" {
            parts.push("Alpha BUY signaal: sterke combinatie van factoren.".to_string());
        } else if row.early == "BUY" {
            parts.push("Vroege koopindicatie.".to_string());
        }

        // Nieuws sentiment
        if row.news_sentiment > 0.7 {
            parts.push(format!("Positieve nieuws sentiment ({:.1}).", row.news_sentiment));
        } else if row.news_sentiment < 0.3 {
            parts.push(format!("Negatieve nieuws sentiment ({:.1}).", row.news_sentiment));
        }

        // Als geen specifieke info, algemene opmerking
        if parts.is_empty() {
            parts.push("Neutrale marktcondities.".to_string());
        }

        // Combineer tot één string
        parts.join(" ")

        // Beperk lengte tot ~200 karakters voor leesbaarheid
        .chars().take(200).collect::<String>()
    }

    fn top10_snapshot(&self) -> Top10Response {
        let rows = self.snapshot();

        // Helper functie om laatste signal_type te vinden
        let get_last_signal_type = |pair: &str| -> String {
            let signals = self.signals.lock().unwrap();
            signals.iter().rev().find(|s| s.pair == pair).map(|s| s.signal_type.clone()).unwrap_or_else(|| "NONE".to_string())
        };

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
                signal_type: get_last_signal_type(&r.pair),
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
                    signal_type: get_last_signal_type(&r.pair),
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

    // NIEUW: Handmatige buy
    fn manual_buy(&self, pair: &str, sl_pct: f64, tp_pct: f64) -> bool {
        let current_price = self.candles.get(pair).and_then(|c| c.close).unwrap_or(0.0);
        if current_price <= 0.0 {
            return false;
        }
        let mut trader = self.trader.lock().unwrap();
        trader.manual_open_position(pair, current_price, sl_pct, tp_pct)
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
    <button class="tab-btn" data-tab="paper_trading">Paper Trading</button>
    <button class="tab-btn" data-tab="backtest">Backtest</button>
    <button class="tab-btn" data-tab="heatmap">Heatmap</button>
    <button class="tab-btn" data-tab="stars">Stars</button>
    <button class="tab-btn" data-tab="news">News</button>
    <button class="tab-btn" data-tab="config">Config</button>
    <button class="tab-btn" data-tab="guide">Guide</button>
  </div>
</header>
<main style="padding:0 8px 8px 8px;">
  <div id="view-markets">
    <div style="margin-bottom:10px;">
      <label for="markets-dir-filter">Filter op DIR:</label>
      <select id="markets-dir-filter">
        <option value="ALL">ALL</option>
        <option value="BUY">BUY</option>
        <option value="SELL">SELL</option>
      </select>
      <label for="markets-stable-filter" style="margin-left:10px;">Include Stablecoins:</label>
      <input type="checkbox" id="markets-stable-filter" checked />
    </div>
    <table id="grid">
      <thead>
        <tr>
          <th>Pair</th><th>Price</th><th>%</th><th>Whale</th>
          <th>Flow</th><th>Dir</th><th>Early</th><th>Alpha</th><th>Pump</th>
          <th>WhPred</th><th>Rel</th><th>News Sent.</th>
          <th>Total score</th><th>Trades</th><th>Buys</th><th>Sells</th>
          <th>O</th><th>H</th><th>L</th><th>C</th>
          <th>Visual</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
  </div>

  <div id="view-signals" style="display:none;">
    <div style="margin-bottom:10px;">
      <label for="signals-dir-filter">Filter op DIR:</label>
      <select id="signals-dir-filter">
        <option value="ALL">ALL</option>
        <option value="BUY">BUY</option>
        <option value="SELL">SELL</option>
      </select>
      <label for="signals-stable-filter" style="margin-left:10px;">Include Stablecoins:</label>
      <input type="checkbox" id="signals-stable-filter" checked />
    </div>
    <table id="signals">
      <thead>
        <tr>
          <th>Time (ts)</th><th>Pair</th><th>Type</th><th>Dir</th>
          <th>Strength</th><th>Flow</th><th>%</th><th>Total score</th>
          <th>Whale</th><th>Vol</th><th>Notional</th><th>Price</th><th>Pump</th>
          <th>Visual</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
  </div>

  <div id="view-top10" style="display:none;">
    <div style="margin-bottom:10px;">
      <label for="top10-dir-filter">Filter op DIR:</label>
      <select id="top10-dir-filter">
        <option value="ALL">ALL</option>
        <option value="BUY">BUY</option>
        <option value="SELL">SELL</option>
      </select>
      <label for="top10-stable-filter" style="margin-left:10px;">Include Stablecoins:</label>
      <input type="checkbox" id="top10-stable-filter" checked />
    </div>
    <h2>🔥 Best 3 Right Now</h2>
    <table id="top3">
      <thead>
        <tr>
          <th>Time</th><th>Pair</th><th>Price</th><th>%</th><th>Flow</th><th>Dir</th>
          <th>Early</th><th>Alpha</th><th>Whale</th><th>Total score</th><th>Pump</th>
          <th>WhPred</th><th>Rel</th><th>Type</th><th>Visual</th><th>Analyse</th>
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
          <th>WhPred</th><th>Rel</th><th>Type</th><th>Visual</th><th>Analyse</th>
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
          <th>WhPred</th><th>Rel</th><th>Type</th><th>Visual</th><th>Analyse</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
  </div>

  <div id="view-paper_trading" style="display:none;">
    <div style="margin-bottom:10px;">
      <label for="paper-stable-filter">Include Stablecoins:</label>
      <input type="checkbox" id="paper-stable-filter" checked />
    </div>
    <h2>Paper Trading Portfolio</h2>
    <div id="portfolio-summary"></div>
    <h3>Manual Buy</h3>
    <div style="margin-bottom:10px;">
      <label>Pair:</label>
      <select id="manual-pair">
        <!-- Vul dynamisch met pairs -->
      </select>
      <label style="margin-left:10px;">SL %:</label>
      <select id="manual-sl">
        <option value="0.5">0.5%</option>
        <option value="1">1%</option>
        <option value="2">2%</option>
        <option value="5">5%</option>
      </select>
      <label style="margin-left:10px;">TP %:</label>
      <select id="manual-tp">
        <option value="1">1%</option>
        <option value="2">2%</option>
        <option value="5">5%</option>
        <option value="10">10%</option>
      </select>
      <button id="manual-buy-btn">Buy</button>
    </div>
    <h3>Open Positions</h3>
    <table id="open-positions">
      <thead>
        <tr><th>Pair</th><th>Entry Price</th><th>Size</th><th>Current Price</th><th>PnL Abs</th><th>PnL %</th><th>Open TS</th></tr>
      </thead>
      <tbody></tbody>
    </table>
    <h3>Equity Curve</h3>
    <canvas id="paper-equity" width="900" height="260" style="border:1px solid #333; background:#111;"></canvas>
    <h3>Trade History</h3>
    <table id="paper-history">
      <thead>
        <tr><th>Pair</th><th>Entry</th><th>Exit</th><th>Size</th><th>PnL</th><th>Reason</th><th>Duration (min)</th></tr>
      </thead>
      <tbody></tbody>
    </table>
  </div>

  <div id="view-backtest" style="display:none;">
    <div style="margin-bottom:10px;">
      <label for="backtest-stable-filter">Include Stablecoins:</label>
      <input type="checkbox" id="backtest-stable-filter" checked />
    </div>
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
    <div style="margin-bottom:10px;">
      <label for="heatmap-stable-filter">Include Stablecoins:</label>
      <input type="checkbox" id="heatmap-stable-filter" checked />
    </div>
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

  <div id="view-stars" style="display:none;">
    <div style="margin-bottom:10px;">
      <label for="stars-stable-filter">Include Stablecoins:</label>
      <input type="checkbox" id="stars-stable-filter" checked />
    </div>
    <h2>⭐ Stars: ANOM & WH_PRED HIGH (last 5 hours)</h2>
    <table id="stars-table">
      <thead>
        <tr>
          <th>Time</th><th>Pair</th><th>Price</th><th>%</th><th>Flow</th><th>Dir</th>
          <th>Early</th><th>Alpha</th><th>Whale</th><th>Total score</th><th>Pump</th>
          <th>WhPred</th><th>Rel</th><th>Type</th><th>Visual</th><th>Analyse</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
  </div>

  <div id="view-news" style="display:none;">
    <div style="margin-bottom:10px;">
      <label for="news-stable-filter">Include Stablecoins:</label>
      <input type="checkbox" id="news-stable-filter" checked />
    </div>
    <h2>📰 News Sentiment</h2>
    <table id="news-table">
      <thead>
        <tr>
          <th>Pair</th><th>Sentiment</th><th>Last Update</th><th>Articles</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
  </div>

  <div id="view-config" style="display:none;">
    <h2>Configuration Settings</h2>
    <form id="config-form">
      <h3>1. Signal Drempels</h3>
      <label>Pump Confidence Threshold (0.0-1.0):</label>
      <input type="number" step="0.1" min="0.0" max="1.0" id="pump_conf_threshold" /><br/>
      <label>Whale Prediction High Threshold (0.0-10.0):</label>
      <input type="number" step="0.1" min="0.0" max="10.0" id="whale_pred_high_threshold" /><br/>
      <label>Early Buy Threshold (0.0-5.0):</label>
      <input type="number" step="0.1" min="0.0" max="5.0" id="early_buy_threshold" /><br/>
      <label>Alpha Buy Threshold (0.0-10.0):</label>
      <input type="number" step="0.1" min="0.0" max="10.0" id="alpha_buy_threshold" /><br/>
      <label>Strong Buy Threshold (0.0-10.0):</label>
      <input type="number" step="0.1" min="0.0" max="10.0" id="strong_buy_threshold" /><br/>
      <label>Whale Min Notional (0.0-10000.0):</label>
      <input type="number" step="100" min="0.0" max="10000.0" id="whale_min_notional" /><br/>
      <label>Anomaly Strength Threshold (0.0-100.0):</label>
      <input type="number" step="1" min="0.0" max="100.0" id="anomaly_strength_threshold" /><br/>

      <h3>2. Score Gewichten</h3>
      <label>Flow Weight (0.0-5.0):</label>
      <input type="number" step="0.1" min="0.0" max="5.0" id="flow_weight" /><br/>
      <label>Price Weight (0.0-5.0):</label>
      <input type="number" step="0.1" min="0.0" max="5.0" id="price_weight" /><br/>
      <label>Whale Weight (0.0-5.0):</label>
      <input type="number" step="0.1" min="0.0" max="5.0" id="whale_weight" /><br/>
      <label>Volume Weight (0.0-5.0):</label>
      <input type="number" step="0.1" min="0.0" max="5.0" id="volume_weight" /><br/>
      <label>Anomaly Weight (0.0-5.0):</label>
      <input type="number" step="0.1" min="0.0" max="5.0" id="anomaly_weight" /><br/>
      <label>Trend Weight (0.0-5.0):</label>
      <input type="number" step="0.1" min="0.0" max="5.0" id="trend_weight" /><br/>

      <h3>3. Paper Trading Instellingen</h3>
      <label>Initial Balance (1000.0-100000.0):</label>
      <input type="number" step="1000" min="1000.0" max="100000.0" id="initial_balance" /><br/>
      <label>Base Notional (10.0-1000.0):</label>
      <input type="number" step="10" min="10.0" max="1000.0" id="base_notional" /><br/>
      <label>Stop Loss Percentage (0.01-0.1):</label>
      <input type="number" step="0.01" min="0.01" max="0.1" id="sl_pct" /><br/>
      <label>Take Profit Percentage (0.01-0.1):</label>
      <input type="number" step="0.01" min="0.01" max="0.1" id="tp_pct" /><br/>
      <label>Max Positions (1-10):</label>
      <input type="number" step="1" min="1" max="10" id="max_positions" /><br/>
      <label>Enable Trading:</label>
      <input type="checkbox" id="enable_trading" /><br/>

      <h3>4. Engine & Data Instellingen</h3>
      <label>WS Workers per Chunk (10-50):</label>
      <input type="number" step="5" min="10" max="50" id="ws_workers_per_chunk" /><br/>
      <label>REST Scan Interval (10-60):</label>
      <input type="number" step="5" min="10" max="60" id="rest_scan_interval_sec" /><br/>
      <label>Cleanup Interval (300-1200):</label>
      <input type="number" step="100" min="300" max="1200" id="cleanup_interval_sec" /><br/>
      <label>Eval Horizon (60-600):</label>
      <input type="number" step="60" min="60" max="600" id="eval_horizon_sec" /><br/>
      <label>Max History (200-1000):</label>
      <input type="number" step="100" min="200" max="1000" id="max_history" /><br/>

      <h3>5. UI & Filter Instellingen</h3>
      <label>Default DIR Filter:</label>
      <select id="default_dir_filter">
        <option value="ALL">ALL</option>
        <option value="BUY">BUY</option>
        <option value="SELL">SELL</option>
      </select><br/>
      <label>Include Stablecoins Default:</label>
      <input type="checkbox" id="include_stablecoins_default" /><br/>
      <label>Heatmap Min Radius (4.0-10.0):</label>
      <input type="number" step="0.5" min="4.0" max="10.0" id="heatmap_min_radius" /><br/>
      <label>Heatmap Max Radius (10.0-20.0):</label>
      <input type="number" step="0.5" min="10.0" max="10.0" id="heatmap_max_radius" /><br/>
      <label>Chart Refresh Rate (0.5-5.0):</label>
      <input type="number" step="0.5" min="0.5" max="5.0" id="chart_refresh_rate_sec" /><br/>

      <h3>6. AI & Self-Learning Instellingen</h3>
      <label>Success Threshold (0.5-1.0):</label>
      <input type="number" step="0.05" min="0.5" max="1.0" id="ai_success_threshold" /><br/>
      <label>Adjustment Step Up (1.0-2.0):</label>
      <input type="number" step="0.01" min="1.0" max="2.0" id="ai_adjustment_step_up" /><br/>
      <label>Adjustment Step Down (0.5-1.0):</label>
      <input type="number" step="0.01" min="0.5" max="1.0" id="ai_adjustment_step_down" /><br/>
      <label>Max Weight (3.0-10.0):</label>
      <input type="number" step="0.5" min="3.0" max="10.0" id="ai_max_weight" /><br/>

      <button type="button" id="save-config">Save Config</button>
      <button type="button" id="reset-config">Reset to Defaults</button>
    </form>
    <div id="config-status"></div>
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
        <li><b>News Sent.</b>: sentiment van recente nieuwsartikelen (0-1).</li>
        <li><b>Visual</b>: link naar de bijbehorende Kraken Pro grafiek.</li>
      </ul>
    </div>
  </div>
</main>
<script>
// ... bestaande JS ...
let activeTab = "markets";

let heatmapPoints = [];
let heatTooltip = null;

const stablecoins = ["USDT", "USDC", "TUSD", "BUSD", "DAI", "UST", "FRAX", "LUSD"];

function isStablecoin(pair) {
  const base = pair.split('/')[0];
  return stablecoins.includes(base);
}

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

// Filter functie voor DIR
function applyDirFilter(tableId, filterSelectId) {
  const filterValue = document.getElementById(filterSelectId).value;
  const tbody = document.querySelector(`#${tableId} tbody`);
  const rows = tbody.querySelectorAll('tr');
  rows.forEach(row => {
    const dirCell = row.cells[5]; // Assuming DIR is the 6th column (index 5)
    if (dirCell) {
      const dirText = dirCell.textContent.trim();
      if (filterValue === 'ALL' || dirText === filterValue) {
        row.style.display = '';
      } else {
        row.style.display = 'none';
      }
    }
  });
}

function switchTab(tab) {
  activeTab = tab;
  document.getElementById("view-markets").style.display =
    tab === "markets" ? "block" : "none";
  document.getElementById("view-signals").style.display =
    tab === "signals" ? "block" : "none";
  document.getElementById("view-top10").style.display =
    tab === "top10" ? "block" : "none";
  document.getElementById("view-paper_trading").style.display =
    tab === "paper_trading" ? "block" : "none";
  document.getElementById("view-backtest").style.display =
    tab === "backtest" ? "block" : "none";
  document.getElementById("view-heatmap").style.display =
    tab === "heatmap" ? "block" : "none";
  document.getElementById("view-stars").style.display =
    tab === "stars" ? "block" : "none";
  document.getElementById("view-news").style.display =
    tab === "news" ? "block" : "none";
  document.getElementById("view-config").style.display =
    tab === "config" ? "block" : "none";
  document.getElementById("view-guide").style.display =
    tab === "guide" ? "block" : "none";

  if (tab === "heatmap") {
    loadHeatmap();
  } else if (tab === "backtest") {
    loadBacktest();
  } else if (tab === "paper_trading") {
    loadPaperTrading();
  } else if (tab === "stars") {
    loadStars();
  } else if (tab === "news") {
    loadNews();
  } else if (tab === "config") {
    loadConfig();
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
  let includeStable = document.getElementById("markets-stable-filter").checked;
  let res = await fetch("/api/stats");
  let data = await res.json();
  let tbody = document.querySelector("#grid tbody");
  tbody.innerHTML = "";

  let filtered = data.filter(r =>
    r.pair.toLowerCase().includes(q) &&
    (includeStable || !isStablecoin(r.pair))
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
      <td>${r.news_sentiment ? r.news_sentiment.toFixed(2) : "0.50"}</td>
      <td>${r.score.toFixed(2)}</td>
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
  applyDirFilter('grid', 'markets-dir-filter');
}

async function loadSignals() {
  let includeStable = document.getElementById("signals-stable-filter").checked;
  let res = await fetch("/api/signals");
  let data = await res.json();
  let tbody = document.querySelector("#signals tbody");
  tbody.innerHTML = "";

  let filtered = data.filter(r => includeStable || !isStablecoin(r.pair));

  for (let r of filtered) {
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
      <td>${r.total_score.toFixed(2)}</td>
      <td>${whaleTxt}</td>
      <td>${r.volume.toFixed(4)}</td>
      <td>${(r.notional/1000).toFixed(1)}k</td>
      <td>${r.price.toFixed(4)}</td>
      <td style="color:${pumpColor}">${pumpText}</td>
      <td>${visual}</td>
    </tr>`;

    tbody.innerHTML += row;
  }
  applyDirFilter('signals', 'signals-dir-filter');
}

async function loadTop10() {
  let includeStable = document.getElementById("top10-stable-filter").checked;
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

    let relClass = "";
    if (r.reliability_label === "HIGH") relClass = "rel_high";
    else if (r.reliability_label === "MEDIUM") relClass = "rel_med";
    else if (r.reliability_label === "LOW") relClass = "rel_low";
    else relClass = "rel_bad";

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
      <td class="${relClass}">${r.reliability_label} (${r.reliability_score.toFixed(0)})</td>
      <td class="signal_type signal_type_${r.signal_type}">${r.signal_type}</td>
      <td>${visual}</td>
      <td>${r.analysis}</td>
    </tr>`;
  }

  for (let r of data.best3.filter(row => includeStable || !isStablecoin(row.pair))) {
    top3Body.innerHTML += renderRow(r);
  }

  for (let r of data.risers.filter(row => includeStable || !isStablecoin(row.pair))) {
    upBody.innerHTML += renderRow(r);
  }

  for (let r of data.fallers.filter(row => includeStable || !isStablecoin(row.pair))) {
    downBody.innerHTML += renderRow(r);
  }
  applyDirFilter('top3', 'top10-dir-filter');
  applyDirFilter('top10-up', 'top10-dir-filter');
  applyDirFilter('top10-down', 'top10-dir-filter');
}

async function loadPaperTrading() {
  let includeStable = document.getElementById("paper-stable-filter").checked;
  // Portfolio overzicht
  let portfolio = await fetch("/api/paper_portfolio").then(r => r.json());
  document.getElementById("portfolio-summary").innerHTML = `
    <p>Balance: €${portfolio.balance.toFixed(2)}</p>
    <p>Realized PnL: €${portfolio.realized_pnl.toFixed(2)}</p>
    <p>Unrealized PnL: €${portfolio.unrealized_pnl.toFixed(2)}</p>
    <p>Positions: ${portfolio.positions}</p>
    <p>Total Trades: ${portfolio.total_trades}</p>
    <p>Winrate: ${portfolio.winrate.toFixed(1)}%</p>
  `;

  // Vul pair dropdown
  let pairs = await fetch("/api/stats").then(r => r.json()).then(d => d.map(r => r.pair));
  let select = document.getElementById("manual-pair");
  // Sla de huidige selectie op voordat we de dropdown opnieuw vullen
  let currentSelection = select.value;
  select.innerHTML = "";
  pairs.forEach(p => {
    let opt = document.createElement("option");
    opt.value = p;
    opt.text = p;
    select.appendChild(opt);
  });
  // Herstel de vorige selectie als deze nog bestaat in de lijst
  if (currentSelection && pairs.includes(currentSelection)) {
    select.value = currentSelection;
  }

  // Open posities
  let positions = await fetch("/api/positions").then(r => r.json());
  let tbody = document.querySelector("#open-positions tbody");
  tbody.innerHTML = "";
  positions.forEach(pos => {
    tbody.innerHTML += `
      <tr>
        <td>${pos.pair}</td>
        <td>${pos.entry_price.toFixed(4)}</td>
        <td>${pos.size.toFixed(4)}</td>
        <td>${pos.current_price.toFixed(4)}</td>
        <td class="${pos.pnl_abs > 0 ? 'pos' : 'neg'}">€${pos.pnl_abs.toFixed(2)}</td>
        <td class="${pos.pnl_pct > 0 ? 'pos' : 'neg'}">${pos.pnl_pct.toFixed(2)}%</td>
        <td>${new Date(pos.open_ts * 1000).toLocaleString()}</td>
      </tr>
    `;
  });

  // Trade history tabel (bestaande)
  let history = await fetch("/api/paper_history").then(r => r.json());
  let histTbody = document.querySelector("#paper-history tbody");
  histTbody.innerHTML = "";
  history.filter(trade => includeStable || !isStablecoin(trade.pair)).forEach(trade => {
    let duration = Math.round((trade.close_ts - trade.open_ts) / 60);
    histTbody.innerHTML += `<tr>
      <td>${trade.pair}</td>
      <td>${trade.entry_price.toFixed(4)}</td>
      <td>${trade.exit_price.toFixed(4)}</td>
      <td>${trade.size.toFixed(4)}</td>
      <td class="${trade.pnl > 0 ? 'pos' : 'neg'}">€${trade.pnl.toFixed(2)}</td>
      <td>${trade.reason}</td>
      <td>${duration}</td>
    </tr>`;
  });

  // Equity curve (bestaande)
  let equity = await fetch("/api/paper_equity").then(r => r.json());
  drawPaperEquity(equity);
}

// Event listener voor Buy knop
document.getElementById("manual-buy-btn").addEventListener("click", async () => {
  let pair = document.getElementById("manual-pair").value;
  let sl_pct = parseFloat(document.getElementById("manual-sl").value);
  let tp_pct = parseFloat(document.getElementById("manual-tp").value);
  let res = await fetch("/api/manual_buy", {
    method: "POST",
    headers: {"Content-Type": "application/json"},
    body: JSON.stringify({pair, sl_pct, tp_pct})
  });
  let result = await res.json();
  if (result.success) {
    alert("Manual buy successful!");
    loadPaperTrading(); // Herlaad
  } else {
    alert("Manual buy failed!");
  }
});

function drawPaperEquity(equity) {
  let canvas = document.getElementById("paper-equity");
  let ctx = canvas.getContext("2d");
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  // Eenvoudige lijn (zoals backtest)
  if (equity.length < 2) return;
  let minY = Math.min(...equity.map(p => p[1]));
  let maxY = Math.max(...equity.map(p => p[1]));
  if (minY === maxY) minY -= 100;
  let padding = 20;
  let w = canvas.width - padding * 2;
  let h = canvas.height - padding * 2;
  ctx.strokeStyle = "#4caf50";
  ctx.lineWidth = 2;
  ctx.beginPath();
  equity.forEach((point, i) => {
    let x = padding + (w * i) / (equity.length - 1);
    let y = padding + h - ((point[1] - minY) / (maxY - minY)) * h;
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  });
  ctx.stroke();
}

async function loadBacktest() {
  let includeStable = document.getElementById("backtest-stable-filter").checked;
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
        <td>${r.avg_win.toFixed(2)}</td>
        <td>${r.avg_loss.toFixed(2)}</td>
        <td>${r.expectancy.toFixed(2)}%</td>
        <td>${r.pnl_sum.toFixed(2)}%</td>
        <td>${r.max_drawdown.toFixed(2)}%</td>
        <td>${r.best_trade.toFixed(2)}</td>
        <td>${r.worst_trade.toFixed(2)}</td>
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
  let includeStable = document.getElementById("heatmap-stable-filter").checked;
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

      for (let p of data.filter(pt => includeStable || !isStablecoin(pt.pair))) {
        const x = x_to_px(p.flow_pct);
        const y = y_to_px(p.pump_score);

        let color = "#4caf50";
        if (p.pump_score >= 8.0 && p.flow_pct >= 80.0) {
          color = "#ff4081";
        } else if (p.pump_score >= 6.0 && p.flow_pct >= 70.0) {
          color = "#00bcd4";
        }

        // REL-based radius and alpha
        let min_rel = 0.0;
        let max_rel = 100.0;
        let rel_norm = (p.reliability_score - min_rel) / (max_rel - min_rel);
        if (rel_norm < 0) rel_norm = 0;
        if (rel_norm > 1) rel_norm = 1;
        let radius = 4 + rel_norm * 8; // 4-12
        let alpha = 0.3 + rel_norm * 0.7; // 0.3-1.0

        ctx.beginPath();
        ctx.globalAlpha = alpha;
        ctx.fillStyle = color;
        ctx.arc(x, y, radius, 0, Math.PI * 2);
        ctx.fill();
        ctx.globalAlpha = 1; // Reset

        heatmapPoints.push({
          x, y,
          pair: p.pair,
          flow: p.flow_pct,
          pump: p.pump_score,
          ts: p.ts,
          color,
          rel: p.reliability_score,
        });
      }
    })
    .catch(err => console.error("heatmap error", err));
}

async function loadStars() {
  let includeStable = document.getElementById("stars-stable-filter").checked;
  let currentTime = Math.floor(Date.now() / 1000);
  let fiveHoursAgo = currentTime - (5 * 3600);
  fetch("/api/top10")
    .then(r => r.json())
    .then(top10Data => {
      let filtered = [];
      // Get pairs with high WH_PRED from risers and fallers
      for (let r of top10Data.risers.concat(top10Data.fallers)) {
        if (r.whale_pred_label === "HIGH" && (includeStable || !isStablecoin(r.pair))) {
          filtered.push(r);
        }
      }
      // Now filter those that have recent ANOM signal within 5 hours
      fetch("/api/signals")
        .then(r => r.json())
        .then(signals => {
          let anomPairs = new Set();
          for (let s of signals) {
            if (s.signal_type === "ANOM" && s.ts >= fiveHoursAgo) {
              anomPairs.add(s.pair);
            }
          }
          let finalFiltered = filtered.filter(r => anomPairs.has(r.pair));
          let tbody = document.querySelector("#stars-table tbody");
          tbody.innerHTML = "";
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

            let predClass = r.whale_pred_label === "HIGH" ? "pred_high" :
              (r.whale_pred_label === "MEDIUM" ? "pred_med" : "pred_low");
            let relClass = r.reliability_label === "HIGH" ? "rel_high" :
              (r.reliability_label === "MEDIUM" ? "rel_med" :
              (r.reliability_label === "LOW" ? "rel_low" : "rel_bad"));
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
              <td class="${relClass}">${r.reliability_label} (${r.reliability_score.toFixed(0)})</td>
              <td class="signal_type signal_type_${r.signal_type}">${r.signal_type}</td>
              <td>${visual}</td>
              <td>${r.analysis}</td>
            </tr>`;
          }
          for (let r of finalFiltered) {
            tbody.innerHTML += renderRow(r);
          }
        });
    })
    .catch(err => console.error("stars error", err));
}

async function loadNews() {
  let includeStable = document.getElementById("news-stable-filter").checked;
  fetch("/api/news")
    .then(r => r.json())
    .then(data => {
      let tbody = document.querySelector("#news-table tbody");
      tbody.innerHTML = "";
      for (let r of data.filter(row => includeStable || !isStablecoin(row.pair))) {
        let sentiment = r.sentiment || 0.5;
        let classSent = sentiment > 0.7 ? "pos" : (sentiment < 0.3 ? "neg" : "");
        tbody.innerHTML += `<tr>
          <td>${r.pair}</td>
          <td class="${classSent}">${sentiment.toFixed(2)}</td>
          <td>${new Date(r.last_update * 1000).toLocaleString()}</td>
          <td>${r.articles}</td>
        </tr>`;
      }
    })
    .catch(err => console.error("news error", err));
}

async function loadConfig() {
  try {
    let res = await fetch("/api/config");
    let cfg = await res.json();
    Object.keys(cfg).forEach(key => {
      const el = document.getElementById(key);
      if (el) {
        if (el.type === 'checkbox') {
          el.checked = cfg[key];
        } else {
          el.value = cfg[key];
        }
      }
    });
  } catch (e) {
    console.error("Config load error:", e);
  }
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

    const R2 = 12*12; // Larger radius for bigger points
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
        `${closest.pair} | ${fmtTime(closest.ts)} | Flow ${closest.flow.toFixed(1)}% | Pump ${closest.pump.toFixed(1)} | REL ${closest.rel.toFixed(0)}`;
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

    const R2 = 12*12;
    if (closest && closestDist <= R2) {
      const search = document.getElementById("search");
      if (search) search.value = closest.pair;
      switchTab("markets");
    }
  });

  // Config event listeners
  document.getElementById('save-config').addEventListener('click', () => {
    const cfg = {};
    const inputs = document.querySelectorAll('#config-form input, #config-form select');
    inputs.forEach(el => {
      if (el.type === 'checkbox') {
        cfg[el.id] = el.checked;
      } else if (el.type === 'number') {
        cfg[el.id] = parseFloat(el.value);
      } else {
        cfg[el.id] = el.value;
      }
    });
    fetch('/api/config', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify(cfg)
    }).then(() => {
      document.getElementById('config-status').textContent = 'Saved successfully!';
      setTimeout(() => document.getElementById('config-status').textContent = '', 3000);
    }).catch(() => {
      document.getElementById('config-status').textContent = 'Save failed!';
    });
  });

  document.getElementById('reset-config').addEventListener('click', () => {
    fetch('/api/config/reset', {method: 'POST'}).then(() => {
      loadConfig();
      document.getElementById('config-status').textContent = 'Reset to defaults!';
      setTimeout(() => document.getElementById('config-status').textContent = '', 3000);
    });
  });
});

// Event listeners voor filters
document.getElementById('markets-dir-filter').addEventListener('change', () => applyDirFilter('grid', 'markets-dir-filter'));
document.getElementById('signals-dir-filter').addEventListener('change', () => applyDirFilter('signals', 'signals-dir-filter'));
document.getElementById('top10-dir-filter').addEventListener('change', () => {
  applyDirFilter('top3', 'top10-dir-filter');
  applyDirFilter('top10-up', 'top10-dir-filter');
  applyDirFilter('top10-down', 'top10-dir-filter');
});

function tick() {
  if (activeTab === "markets") {
    loadMarkets();
  } else if (activeTab === "signals") {
    loadSignals();
  } else if (activeTab === "top10") {
    loadTop10();
  } else if (activeTab === "paper_trading") {
    loadPaperTrading();
  } else if (activeTab === "backtest") {
    loadBacktest();
  } else if (activeTab === "news") {
    loadNews();
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
// HOOFDSTUK 16 – NIEUWS-SENTIMENT SCANNER (NIEUW STAP)
// ============================================================================

// NIEUW: run_news_scanner functie (stap 2)
async fn run_news_scanner(engine: Engine) -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting news sentiment scanner...");

    loop {
        // Voorbeeld: RSS feed van een crypto nieuws site (bijv. CoinDesk)
        let rss_url = "https://cointelegraph.com/rss";

        if let Ok(resp) = reqwest::get(rss_url).await {
            if let Ok(content) = resp.text().await {
                if let Ok(channel) = Channel::read_from(Cursor::new(content.as_bytes())) {
                    for item in channel.items {
                        if let Some(title) = item.title {
                            // Eenvoudige sentiment analyse: tel positieve/negatieve woorden
                            let positive_words = SENTIMENT_MAP.get("positive").cloned().unwrap_or_default();
                            let negative_words = SENTIMENT_MAP.get("negative").cloned().unwrap_or_default();

                            let title_lower = title.to_lowercase();
                            let mut pos_score = 0.0;
                            let mut neg_score = 0.0;
                            for (word, weight) in &positive_words {
                                pos_score += title_lower.matches(word).count() as f64 * *weight as f64;
                            }
                            for (word, weight) in &negative_words {
                                neg_score += title_lower.matches(word).count() as f64 * *weight as f64;
                            }
                            let sentiment = if pos_score + neg_score > 0.0 {
                                pos_score / (pos_score + neg_score)
                            } else {
                                0.5
                            };

                            // Extract pair van title (bijv. "BTC" of "Bitcoin")
                            if let Some(pair) = extract_pair_from_title(&title) {
                                engine.update_sentiment(&pair, sentiment, &title);
                                println!("[NEWS] {} sentiment {:.2} for {}", title, sentiment, pair);
                            } else {
                                engine.update_sentiment("BTC/EUR", sentiment, &title);
                                println!("[NEWS] {} sentiment {:.2} for BTC/EUR (general)", title, sentiment);
                            }
                        }
                    }
                }
            }
        }

        // Wacht 1 minuut voor volgende scan
        sleep(Duration::from_secs(60)).await;
    }
}

// NIEUW: Helper functie om pair uit title te extraheren
fn extract_pair_from_title(title: &str) -> Option<String> {
    let title_lower = title.to_lowercase();

    // Use pre-sorted keywords to check more specific keywords first
    for (keyword, pair) in SORTED_KEYWORDS.iter() {
        if title_lower.contains(keyword) {
            return Some(pair.clone());
        }
    }
    None
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


async fn run_http(engine: Engine, config: Arc<Mutex<AppConfig>>) {
    let engine_filter = warp::any().map(move || engine.clone());
    let config_filter = warp::any().map(move || config.clone());

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

    let api_paper_portfolio = warp::path!("api" / "paper_portfolio")
        .and(engine_filter.clone())
        .map(|engine: Engine| {
            let trader = engine.trader.lock().unwrap();
            warp::reply::json(&PaperPortfolioResponse {
                balance: trader.balance,
                realized_pnl: trader.realized_pnl,
                unrealized_pnl: trader.balance - trader.initial_balance - trader.realized_pnl,
                positions: trader.positions.len(),
                total_trades: trader.trade_history.len(),
                winrate: if trader.trade_history.is_empty() {
                    0.0
                } else {
                    trader.trade_history.iter().filter(|t| t.pnl > 0.0).count() as f64 / trader.trade_history.len() as f64 * 100.0
                },
            })
        });

    let api_paper_history = warp::path!("api" / "paper_history")
        .and(engine_filter.clone())
        .map(|engine: Engine| {
            let trader = engine.trader.lock().unwrap();
            warp::reply::json(&trader.trade_history)
        });

    let api_paper_equity = warp::path!("api" / "paper_equity")
        .and(engine_filter.clone())
        .map(|engine: Engine| {
            let trader = engine.trader.lock().unwrap();
            warp::reply::json(&trader.equity_curve)
        });

    let api_positions = warp::path!("api" / "positions")
        .and(engine_filter.clone())
        .map(|engine: Engine| warp::reply::json(&engine.positions_snapshot()));

    let api_config_get = warp::path!("api" / "config")
        .and(config_filter.clone())
        .map(|config: Arc<Mutex<AppConfig>>| {
            let cfg = config.lock().unwrap();
            warp::reply::json(&*cfg)
        });

    let api_config_post = warp::path!("api" / "config")
        .and(config_filter.clone())
        .and(warp::body::json())
        .map(|config: Arc<Mutex<AppConfig>>, new_cfg: AppConfig| {
            *config.lock().unwrap() = new_cfg.clone();
            let _ = save_config(&new_cfg);
            warp::reply::json(&serde_json::json!({"status": "saved"}))
        });

    let api_config_reset = warp::path!("api" / "config" / "reset")
        .and(config_filter.clone())
        .map(|config: Arc<Mutex<AppConfig>>| {
            let default = AppConfig::default();
            *config.lock().unwrap() = default.clone();
            let _ = save_config(&default);
            warp::reply::json(&serde_json::json!({"status": "reset"}))
        });

    // NIEUW: API voor nieuws-sentiment (stap 4)
    let api_news = warp::path!("api" / "news")
        .and(engine_filter.clone())
        .map(|engine: Engine| {
            let mut news_data = Vec::new();
            for ns in engine.news_sentiment.iter() {
                let pair = ns.key().clone();
                let value = ns.value();
                let sentiment = value.0;
                let last_update = value.1;
                let title = value.2.clone();
                news_data.push(serde_json::json!({
                    "pair": pair,
                    "sentiment": sentiment,
                    "last_update": last_update,
                    "articles": title
                }));
            }
            warp::reply::json(&news_data)
        });

    let api_manual_buy = warp::path!("api" / "manual_buy")
        .and(warp::body::json())
        .and(engine_filter.clone())
        .map(|body: serde_json::Value, engine: Engine| {
            let pair = body["pair"].as_str().unwrap_or("");
            let sl_pct = body["sl_pct"].as_f64().unwrap_or(0.0);
            let tp_pct = body["tp_pct"].as_f64().unwrap_or(0.0);
            let success = engine.manual_buy(pair, sl_pct, tp_pct);
            warp::reply::json(&serde_json::json!({"success": success}))
        });

    let index = warp::path::end().map(|| warp::reply::html(DASHBOARD_HTML));

    let routes = api_stats
        .or(api_signals)
        .or(api_top10)
        .or(api_heatmap)
        .or(api_backtest)
        .or(api_paper_portfolio)
        .or(api_paper_history)
        .or(api_paper_equity)
        .or(api_positions)
        .or(api_config_get)
        .or(api_config_post)
        .or(api_config_reset)
        .or(api_news)
        .or(api_manual_buy)
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

    let config = Arc::new(Mutex::new(load_config().await));
    let engine = Engine::new();
    let engine_for_ws = engine.clone();

    // Clone chunks for orderbook workers
    let ob_chunks: Vec<Vec<String>> = ws_pairs.chunks(20).map(|c| c.to_vec()).collect();

    // Spawn trade WebSocket workers
    for (i, chunk) in chunks.into_iter().enumerate() {
        let e = engine_for_ws.clone();
        let _cfg = config.clone();  // Niet gebruikt, maar clone om warning te vermijden
        tokio::spawn(async move {
            if let Err(err) = run_kraken_worker(e, chunk, i).await {
                eprintln!("WS worker {} terminated with error: {:?}", i, err);
            }
        });
        // Vertraging tussen workers om rate limiting te voorkomen
        sleep(Duration::from_millis(500)).await;
    }

    // Spawn orderbook WebSocket workers
    let engine_for_ob = engine.clone();
    for (i, chunk) in ob_chunks.into_iter().enumerate() {
        let e = engine_for_ob.clone();
        let _cfg = config.clone();  // Niet gebruikt, maar clone om warning te vermijden
        tokio::spawn(async move {
            if let Err(err) = run_orderbook_worker(e, chunk, i).await {
                eprintln!("OB worker {} terminated with error: {:?}", i, err);
            }
        });
        // Vertraging tussen workers
        sleep(Duration::from_millis(500)).await;
    }

    let engine_http = engine.clone();
    let config_http = config.clone();
    let engine_anom = engine.clone();
    let engine_eval = engine.clone();
    let engine_cleanup = engine.clone();
    let engine_news = engine.clone(); // NIEUW: Voor news scanner (stap 2)

    tokio::join!(
        async move { run_http(engine_http, config_http).await; },
        async move {
            let _ = run_anomaly_scanner(engine_anom, kraken_keys, key_to_norm).await;
        },
        async move { run_self_evaluator(engine_eval).await; },
        async move { run_cleanup(engine_cleanup).await; },
        async move { run_news_scanner(engine_news).await; }, // NIEUW: Toegevoegd (stap 2)
    );

    Ok(())
}
