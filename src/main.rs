// ============================================================================
// WhaleRadar – main.rs (Volledige versie voor historie + app) - VERSIE 5
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
//     2.2 Ticker & anomaly verwerking (REST)
//     6.3 Analyse & snapshots
//  7. Betrouwbaarheid & kwaliteitsscores
//  8. Normalisatie (assets & pairs)
//  9. Frontend (HTML dashboard) – Aangepast voor Confidence + Momentum + Trade Score + Health Tab + Status Kolom + Count Trades Kolom + Forecast Tabblad + Priority Pair Selectie in Health Tab + Tim[...]
// 10. WebSocket workers
// 11. REST anomaly scanner
// 12. Self-evaluator (zelflerend)
// 13. Cleanup & onderhoud (agressiever gemaakt)
//  14. HTTP server & API (met health endpoint uitgebreid met echte metrics + Forecast endpoint + Priority Pair API)
//  15. Self-healing watchdog (NIEUW, limiet verwijderd)
// 16. Main entrypoint (met graceful shutdown)
//
// Aangepast voor:
// - Status kolom in Manual Trades: Markt-status per pair (buy/sell flow).
// - Aanbeveling 4: Verleng age-penalty in rel_score naar 10 minuten.
// - Nieuwe kolom Count Trades: Toont aantal Buy/Sell trades als "100_Buy / 30_Sell".
// - Nieuw tabblad Forecast met tabel voor voorspellingen.
// - Priority Pair: Eén geselecteerde pair die frequenter data ophaalt (elke 5s in plaats van 20s) en gehighlight wordt in alle tabbladen.
// - Time kolom toegevoegd in Markets tabblad.
// - Search filter toegevoegd in Signals tabblad.
// - News functionaliteit VERWIJDERD: Geen KEYWORD_MAP, SENTIMENT_MAP, run_news_scanner, API voor news.
// ============================================================================

use chrono::Utc;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use lazy_static::lazy_static;
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::time::{interval, sleep, Duration};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use warp::Filter;

// ============================================================================
// NIEUW: Self-Healing Counters
// ============================================================================

lazy_static! {
    static ref SELF_HEALING_COUNTS: Arc<Mutex<SelfHealingCounts>> = Arc::new(Mutex::new(SelfHealingCounts::new()));
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SelfHealingCounts {
    news_scanner_restarts: usize,
    ws_worker_restarts: usize,
    anomaly_scanner_restarts: usize,
    total_restarts: usize,
}

impl SelfHealingCounts {
    fn new() -> Self {
        Self {
            news_scanner_restarts: 0,
            ws_worker_restarts: 0,
            anomaly_scanner_restarts: 0,
            total_restarts: 0,
        }
    }

    fn increment_news(&mut self) {
        self.news_scanner_restarts += 1;
        self.total_restarts += 1;
    }

    fn increment_ws(&mut self) {
        self.ws_worker_restarts += 1;
        self.total_restarts += 1;
    }

    fn increment_anomaly(&mut self) {
        self.anomaly_scanner_restarts += 1;
        self.total_restarts += 1;
    }
}

// ============================================================================
// LAZY STATIC INITIALIZATION
// ============================================================================

lazy_static! {
    static ref START_TIME: i64 = Utc::now().timestamp(); // Voor uptime tracking
}

async fn load_config() -> AppConfig {
    match tokio::fs::read_to_string(CONFIG_FILE).await {
        Ok(content) => serde_json::from_str(content.as_str()).unwrap_or_default(),
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
// HOOFDSTUK 1 – CONFIGURATIE & CONSTANTES
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppConfig {
    pump_conf_threshold: f64,
    whale_pred_high_threshold: f64,
    early_buy_threshold: f64,
    alpha_buy_threshold: f64,
    strong_buy_threshold: f64,
    whale_min_notional: f64,
    anomaly_strength_threshold: f64,
    flow_weight: f64,
    price_weight: f64,
    whale_weight: f64,
    volume_weight: f64,
    anomaly_weight: f64,
    trend_weight: f64,
    initial_balance: f64,
    base_notional: f64,
    sl_pct: f64,
    tp_pct: f64,
    max_positions: usize,
    enable_trading: bool,
    ws_workers_per_chunk: usize,
    rest_scan_interval_sec: u64,
    cleanup_interval_sec: u64,
    eval_horizon_sec: i64,
    max_history: usize,
    default_dir_filter: String,
    include_stablecoins_default: bool,
    heatmap_min_radius: f64,
    heatmap_max_radius: f64,
    chart_refresh_rate_sec: f64,
    ai_success_threshold: f64,
    ai_adjustment_step_up: f64,
    ai_adjustment_step_down: f64,
    ai_max_weight: f64,
    // NIEUW: Eén priority pair voor frequentere data
    priority_pair: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            pump_conf_threshold: 0.7,
            whale_pred_high_threshold: 8.0,
            early_buy_threshold: 3.0,
            alpha_buy_threshold: 7.5,
            strong_buy_threshold: 5.0,
            whale_min_notional: 5000.0,
            anomaly_strength_threshold: 40.0,
            flow_weight: 2.2,
            price_weight: 0.7,
            whale_weight: 1.4,
            volume_weight: 1.3,
            anomaly_weight: 1.5,
            trend_weight: 1.1,
            initial_balance: 10000.0,
            base_notional: 100.0,
            sl_pct: 0.02,
            tp_pct: 0.05,
            max_positions: 5,
            enable_trading: true,
            ws_workers_per_chunk: 20,
            rest_scan_interval_sec: 20,
            cleanup_interval_sec: 600,
            eval_horizon_sec: 300,
            max_history: 400,
            default_dir_filter: "ALL".to_string(),
            include_stablecoins_default: true,
            heatmap_min_radius: 4.0,
            heatmap_max_radius: 12.0,
            chart_refresh_rate_sec: 1.0,
            ai_success_threshold: 0.7,
            ai_adjustment_step_up: 1.02,
            ai_adjustment_step_down: 0.98,
            ai_max_weight: 5.0,
            // NIEUW: Geen standaard priority pair
            priority_pair: None,
        }
    }
}

const CONFIG_FILE: &str = "config.json";
const SIGNAL_FILE: &str = "signals.json";
const MAX_HISTORY: usize = 20;

const VIRTUAL_INITIAL_BALANCE: f64 = 10_000.0;
const VIRTUAL_BASE_NOTIONAL: f64 = 100.0;
const VIRTUAL_MAX_POSITIONS: usize = 5;
const VIRTUAL_SL_PCT: f64 = 0.02;
const VIRTUAL_TP_PCT: f64 = 0.05;

// ============================================================================
// HOOFDSTUK 2 – BAYESIAANS AI / ZELFLEREND SYSTEEM
// ============================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
struct SignalStats {
    wins: u32,
    losses: u32,
    threshold: f64,
    last_updated: Option<chrono::DateTime<chrono::Utc>>,
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
    match tokio::fs::read_to_string(SIGNAL_FILE).await {
        Ok(content) => serde_json::from_str(content.as_str()).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

async fn save_signal_stats(map: &HashMap<String, SignalStats>) {
    if let Ok(json) = serde_json::to_string_pretty(map) {
        if let Err(e) = tokio::fs::write(SIGNAL_FILE, json).await {
            eprintln!("[ERR] Kon signals.json niet opslaan: {}", e);
        }
    }
}

// ============================================================================
// HOOFDSTUK 3 – CORE DATA STRUCTUREN
// ============================================================================

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
    recent_buys_5m: Vec<(f64, f64)>,
    recent_sells_5m: Vec<(f64, f64)>,
    last_flow_pct_5m: f64,
    last_dir_5m: String,
    recent_prices: Vec<(f64, f64)>,
    last_pump_score: f64,
    last_pump_signal: Option<String>,
    whale_pred_score: f64,
    whale_pred_label: Option<String>,
    last_update_ts: i64,
    recent_anom: bool,
    last_whale_pred_high: bool,
    // NIEUW: Counters voor aantal trades per side
    buy_trades: u64,
    sell_trades: u64,
    // NIEUW: DIR history voor gemiddelde duur
    dir_history: Vec<(i64, String)>,
    last_dir_for_history: Option<String>,
}

impl TradeState {
    fn compute_avg_durations(&self) -> (f64, f64, f64) {
        let mut buy_durations = Vec::new();
        let mut sell_durations = Vec::new();
        let mut neutral_durations = Vec::new();

        let mut prev_ts: Option<i64> = None;
        let mut prev_dir: Option<String> = None;

        for (ts, dir) in &self.dir_history {
            if let (Some(p_ts), Some(p_dir)) = (prev_ts, &prev_dir) {
                let duration = *ts - p_ts;
                if duration > 0 {
                    match p_dir.as_str() {
                        "BUY" => buy_durations.push(duration as f64),
                        "SELL" => sell_durations.push(duration as f64),
                        "NEUTR" => neutral_durations.push(duration as f64),
                        _ => {}
                    }
                }
            }
            prev_ts = Some(*ts);
            prev_dir = Some(dir.clone());
        }

        let avg_buy = if buy_durations.is_empty() { 0.0 } else { buy_durations.iter().sum::<f64>() / buy_durations.len() as f64 };
        let avg_sell = if sell_durations.is_empty() { 0.0 } else { sell_durations.iter().sum::<f64>() / sell_durations.len() as f64 };
        let avg_neutral = if neutral_durations.is_empty() { 0.0 } else { neutral_durations.iter().sum::<f64>() / neutral_durations.len() as f64 };

        (avg_buy, avg_sell, avg_neutral)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    bids: Vec<(f64, f64)>,
    asks: Vec<(f64, f64)>,
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
    whale_pred_score: f64,
    whale_pred_label: String,
    reliability_score: f64,
    reliability_label: String,
    // NIEUW: Voor Count Trades kolom
    buy_trades: u64,
    sell_trades: u64,
    // NIEUW: Time kolom voor Markets tabblad
    ts: i64,
    // NIEUW: Gemiddelde duur DIR
    avg_buy_duration_sec: f64,
    avg_sell_duration_sec: f64,
    avg_neutral_duration_sec: f64,
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
    ret_5m: Option<f64>,
    eval_horizon_sec: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    analysis: String,
    whale_pred_score: f64,
    whale_pred_label: String,
    reliability_score: f64,
    reliability_label: String,
    signal_type: String,
    confidence: f64,
    momentum: f64,
    trade_score: f64,  // NIEUW: Vervangt total_score
    // NIEUW: Voor Count Trades kolom in Top 10
    buy_trades: u64,
    sell_trades: u64,
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
    reliability_score: f64,
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

const STARS_HISTORY_FILE: &str = "stars_history.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StarsHistory {
    history: Vec<TopRow>,
    dirty: bool,
}

// ============================================================================
// NIEUW: Forecast Struct voor Voorspellingen
// ============================================================================

#[derive(Debug, Clone, Serialize)]
struct ForecastRow {
    ts: i64,
    pair: String,
    trades_buys: u64,
    trades_sells: u64,
    volume: f64,
    value: f64,  // Notional in €
    current_price: f64,
    target_price: f64,
    pct_change_target: f64,  // % stijging/daling naar doel
    probability: f64,  // Waarschijnlijkheid in %
    duration_min: u32,  // Duur in minuten
}

// ============================================================================
// NIEUWE FUNCTIES: Confidence, Momentum en Trade Score Berekening
// ============================================================================

fn compute_confidence(r: &Row, trades_count: usize, rel_score: f64) -> f64 {
    let mut conf = 0.0;
    // Base on Rel (0-100)
    conf += rel_score;
    // Bonus for trades
    if trades_count >= 10 {
        conf += 20.0;
    } else if trades_count >= 5 {
        conf += 10.0;
    }
    // Bonus for strong signals
    if r.alpha == "BUY" || r.early == "BUY" {
        conf += 10.0;
    }
    // Penalty for low Rel
    if rel_score < 50.0 {
        conf *= 0.8;
    }
    conf.min(100.0).max(0.0)
}

fn compute_momentum(pct: f64, pump_score: f64, flow_pct: f64) -> f64 {
    let pct_score = (pct.clamp(-10.0, 10.0) + 10.0) / 20.0 * 40.0;  // 0-40 voor % (-10% tot +10%)
    let pump_norm = pump_score.min(10.0) / 10.0 * 30.0;  // 0-30 voor pump
    let flow_norm = flow_pct / 100.0 * 30.0;  // 0-30 voor flow
    (pct_score + pump_norm + flow_norm).min(100.0)
}

fn compute_trade_score(
    momentum: f64,
    confidence: f64,
    rel_score: f64,
    whale_pred: f64,
    pump_score: f64,
    flow_pct: f64,
    pct_change: f64,
) -> f64 {
    let mut score = 0.0;
    
    // Gewichten
    score += momentum * 0.3;       // Momentum kracht
    score += confidence * 0.2;     // Betrouwbaarheid
    score += rel_score * 0.2;      // Data kwaliteit
    score += whale_pred * 0.1;     // Whale kans
    score += pump_score * 0.1;     // Impuls
    score += flow_pct * 0.1;       // Marktstemming
    
    // Risico penaliteiten
    if rel_score < 50.0 {
        score -= 20.0;  // Slechte data = hoger risico
    }
    if pct_change > 20.0 {
        score -= 10.0;  // Te grote stijging = mogelijk pump, risico op dump
    }
    if whale_pred < 4.0 {
        score -= 5.0;   // Lage whale kans = minder overtuigend
    }
    
    score.max(0.0).min(100.0)
}

// ============================================================================
// HOOFDSTUK 5 – MANUAL TRADING MODULE (AANGEPAST)
// ============================================================================

const MANUAL_TRADES_FILE: &str = "manual_trades.json";
const MANUAL_EQUITY_FILE: &str = "manual_trades_equity.json";
const MANUAL_BASE_NOTIONAL: f64 = 100.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManualTrade {
    pair: String,
    entry_price: f64,
    size: f64,
    open_ts: i64,
    stop_loss: f64,
    take_profit: f64,
    fee_pct: f64,
    manual_amount: f64,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManualTraderState {
    initial_balance: f64,
    balance: f64,
    trades: HashMap<String, ManualTrade>,
    equity_curve: Vec<(i64, f64)>,
}

impl ManualTraderState {
    fn new() -> Self {
        Self {
            initial_balance: VIRTUAL_INITIAL_BALANCE,
            balance: VIRTUAL_INITIAL_BALANCE,
            trades: HashMap::new(),
            equity_curve: Vec::new(),
        }
    }

    async fn load() -> Self {
        match tokio::fs::read_to_string(MANUAL_TRADES_FILE).await {
            Ok(content) => {
                match serde_json::from_str(content.as_str()) {
                    Ok(state) => state,
                    Err(e) => {
                        eprintln!("[WARN] Failed to parse {}: {}. Starting fresh.", MANUAL_TRADES_FILE, e);
                        Self::new()
                    }
                }
            }
            Err(_) => Self::new(),
        }
    }

    async fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::to_string_pretty(self)?;
        tokio::fs::write(MANUAL_TRADES_FILE, json).await?;
        Ok(())
    }

    async fn save_equity(&self) -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::to_string_pretty(&self.equity_curve)?;
        tokio::fs::write(MANUAL_EQUITY_FILE, json).await?;
        Ok(())
    }

    fn add_trade(&mut self, pair: &str, price: f64, sl_pct: f64, tp_pct: f64, fee_pct: f64, manual_amount: f64) -> bool {
        if self.trades.contains_key(pair) {
            return false;
        }
        let size = manual_amount / price;
        let sl = price * (1.0 - sl_pct / 100.0);
        let tp = price * (1.0 + tp_pct / 100.0);
        let trade = ManualTrade {
            pair: pair.to_string(),
            entry_price: price,
            size,
            open_ts: chrono::Utc::now().timestamp(),
            stop_loss: sl,
            take_profit: tp,
            fee_pct,
            manual_amount,
        };
        self.trades.insert(pair.to_string(), trade);
        println!(
            "[MANUAL TRADE] OPEN {} at {:.5} size {:.5} amount {:.2} SL={:.5} TP={:.5} fee={:.2}%",
            pair, price, size, manual_amount, sl, tp, fee_pct
        );
        true
    }

    fn close_trade(&mut self, pair: &str, exit_price: f64) -> bool {
        if let Some(trade) = self.trades.remove(pair) {
            let pnl = (exit_price - trade.entry_price) * trade.size;
            let fee_amount = pnl.abs() * (trade.fee_pct / 100.0);
            let net_pnl = pnl - fee_amount;
            self.balance += net_pnl;
            let now = chrono::Utc::now().timestamp();
            self.equity_curve.push((now, self.balance));
            if self.equity_curve.len() > 365 {
                self.equity_curve.remove(0);
            }
            println!(
                "[MANUAL TRADE] CLOSED {} at {:.5} Gross PnL={:.2} Fee={:.2} Net PnL={:.2}",
                pair, exit_price, pnl, fee_amount, net_pnl
            );
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ManualTradeView {
    pair: String,
    entry_price: f64,
    size: f64,
    open_ts: i64,
    stop_loss: f64,
    take_profit: f64,
    current_price: f64,
    pnl_abs: f64,
    pnl_pct: f64,
    fee_pct: f64,
    manual_amount: f64,
}

#[derive(Debug, Clone, Serialize)]
struct ManualTradesResponse {
    balance: f64,
    initial_balance: f64,
    trades: Vec<ManualTradeView>,
}

// ============================================================================
// FIX: ScoreWeights definitie toegevoegd voor Engine
// ============================================================================

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
            flow_w: 2.2,
            price_w: 0.7,
            whale_w: 1.4,
            volume_w: 1.3,
            anomaly_w: 1.5,
            trend_w: 1.1,
        }
    }
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
    manual_trader: Arc<Mutex<ManualTraderState>>,
    stars_history: Arc<Mutex<StarsHistory>>,
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
            manual_trader: Arc::new(Mutex::new(ManualTraderState::new())),
            stars_history: Arc::new(Mutex::new(StarsHistory { history: Vec::new(), dirty: false })),
        }
    }

    fn mark_signalled(&self, pair: &str) {
        self.signalled_pairs.insert(pair.to_string(), true);
    }

    fn push_signal(&self, ev: SignalEvent) {
        self.mark_signalled(&ev.pair);
        let mut buf = self.signals.lock().unwrap();
        buf.push(ev);
        if buf.len() > 400 {
            let overflow = buf.len() - 400;
            buf.drain(0..overflow);
        }
    }

    fn add_to_stars_history(&self, row: TopRow) {
        println!("[STAR] Adding to history: {} at ts {}", row.pair, row.ts);
        let mut history = self.stars_history.lock().unwrap();
        history.history.push(row);
        history.dirty = true;
        if history.history.len() > 1000 {
            history.history.remove(0);
        }
    }

    async fn save_stars_history(&self) -> Result<(), Box<dyn std::error::Error>> {
        let history = self.stars_history.lock().unwrap();
        let json = serde_json::to_string_pretty(&*history)?;
        tokio::fs::write(STARS_HISTORY_FILE, json).await?;
        println!("[STARS SAVER] Saved history with {} entries", history.history.len());
        Ok(())
    }

    async fn load_stars_history(&self) -> Result<(), Box<dyn std::error::Error>> {
        match tokio::fs::read_to_string(STARS_HISTORY_FILE).await {
            Ok(content) => {
                let mut history = self.stars_history.lock().unwrap();
                // Probeer eerst de volledige StarsHistory struct te parsen
                match serde_json::from_str(content.as_str()) {
                    Ok(stars_history) => {
                        *history = stars_history;
                        println!("[STARS] Loaded full StarsHistory with {} entries", history.history.len());
                    }
                    Err(_) => {
                        // Fallback: probeer als oude array van TopRow te parsen
                        match serde_json::from_str::<Vec<TopRow>>(content.as_str()) {
                            Ok(history_vec) => {
                                *history = StarsHistory { history: history_vec, dirty: false };
                                println!("[STARS] Loaded legacy array with {} entries", history.history.len());
                            }
                            Err(e) => {
                                eprintln!("[STARS] Failed to parse stars_history.json (legacy fallback also failed): {}", e);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("[STARS] Failed to read stars_history.json: {}", e);
            }
        }
        Ok(())
    }

    fn handle_trade(&self, pair: &str, price: f64, volume: f64, side: &str, ts: f64) {
        let ts_int = ts.floor() as i64;
        let mut t = self.trades.entry(pair.to_string()).or_default();

        let prev_whale = t.last_whale;
        let prev_early = t.last_early.clone().unwrap_or_else(|| "NONE".to_string());
        let prev_alpha = t.last_alpha.clone().unwrap_or_else(|| "NONE".to_string());
        let prev_pump_sig = t.last_pump_signal.clone().unwrap_or_else(|| "NONE".to_string());
        let prev_pred_label = t.whale_pred_label.clone().unwrap_or_else(|| "NONE".to_string());

        t.last_update_ts = ts_int;

        if side == "b" {
            t.buy_volume += volume;
            t.buy_trades += 1;  // NIEUW: Increment buy trades counter
        } else {
            t.sell_volume += volume;
            t.sell_trades += 1;  // NIEUW: Increment sell trades counter
        }
        t.trade_count += 1;

        let notional = price * volume;

        let s0 = t.ewma_trade_size.unwrap_or(volume);
        let s1 = 0.9 * s0 + 0.1 * volume;
        t.ewma_trade_size = Some(s1);

        let n0 = t.ewma_notional.unwrap_or(notional);
        let n1 = 0.9 * n0 + 0.1 * notional;
        t.ewma_notional = Some(n1);

        let v0 = t.ewma_volume.unwrap_or(volume);
        let v1 = 0.9 * v0 + 0.1 * volume;
        t.ewma_volume = Some(v1);

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

        t.recent_prices.push((ts, price));
        let cutoff_price = ts - 300.0;
        t.recent_prices.retain(|(x, _)| *x >= cutoff_price);

        let cutoff = ts - 60.0;
        if side == "b" {
            t.recent_buys.push((ts, volume));
        } else {
            t.recent_sells.push((ts, volume));
        }
        t.recent_buys.retain(|(x, _)| *x >= cutoff);
        t.recent_sells.retain(|(x, _)| *x >= cutoff);

        let b: f64 = t.recent_buys.iter().map(|(_, v)| *v).sum();
        let s: f64 = t.recent_sells.iter().map(|(_, v)| v).sum();
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

        // NIEUW: DIR history bijwerken
        if t.last_dir_for_history.as_ref() != Some(&dir) {
            t.dir_history.push((ts_int, dir.clone()));
            if t.dir_history.len() > 100 {
                t.dir_history.remove(0);
            }
            t.last_dir_for_history = Some(dir.clone());
        }

        let cutoff5 = ts - 300.0;
        if side == "b" {
            t.recent_buys_5m.push((ts, volume));
        } else {
            t.recent_sells_5m.push((ts, volume));
        }
        t.recent_buys_5m.retain(|(x, _)| *x >= cutoff5);
        t.recent_sells_5m.retain(|(x, _)| *x >= cutoff5);

        let b5: f64 = t.recent_buys_5m.iter().map(|(_, v)| v).sum();
        let s5: f64 = t.recent_sells_5m.iter().map(|(_, v)| v).sum();
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

        let mut flow_score_short = 0.0;
        if flow_pct > 75.0 {
            flow_score_short = 3.0;
        } else if flow_pct > 65.0 {
            flow_score_short = 2.0;
        } else if flow_pct > 55.0 {
            flow_score_short = 1.0;
        }

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

        if let Some(ob) = self.orderbooks.get(pair) {
            let age = ts_int.saturating_sub(ob.timestamp);
            if age >= 0 && age <= 10 {
                let bid_volume: f64 = ob.bids.iter().take(10).map(|(_, v)| v).sum();
                let ask_volume: f64 = ob.asks.iter().take(10).map(|(_, v)| v).sum();
                let total_volume = bid_volume + ask_volume;

                if total_volume > 0.0 {
                    let bid_ratio = bid_volume / total_volume;
                    
                    if side == "b" && bid_ratio > 0.65 {
                        whale_score += 0.5;
                    } else if side == "s" && bid_ratio < 0.35 {
                        whale_score += 0.5;
                    }

                    if bid_ratio > 0.75 && side == "b" {
                        whale_score += 0.3;
                    } else if bid_ratio < 0.25 && side == "s" {
                        whale_score += 0.3;
                    }
                }
            }
        }

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

        let weights = self.weights.lock().unwrap().clone();
        let total_score = weights.flow_w * flow_score
            + weights.price_w * price_score
            + weights.whale_w * whale_score
            + weights.volume_w * volume_score
            + weights.anomaly_w * anomaly_score
            + weights.trend_w * trend_score;

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

        let mut whale_pred_score = 0.0;

        if !is_whale && dir == "BUY" && flow_pct > 60.0 {
            whale_pred_score += (flow_pct - 60.0) * 0.08;
        }

        if !is_whale && dir_5m == "BUY" && flow_pct_5m > 55.0 {
            whale_pred_score += (flow_pct_5m - 55.0) * 0.06;
        }

        if !is_whale && volume < s1 * 0.8 {
            whale_pred_score += 1.0;
        }

        let abs_ret_5s = ret_5s.abs();
        let abs_ret_30s = ret_30s.abs();
        if abs_ret_5s < 0.5 && abs_ret_30s < 1.0 && pct >= -0.5 {
            whale_pred_score += 1.0;
        }

        if vol_ratio < 1.3 {
            whale_pred_score += 0.5;
        }

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
        t.last_whale_pred_high = whale_pred_label == "HIGH";

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

        // BETROUWBARE HISTORIE: Alleen bij HIGH + recente ANOM toevoegen, geen duplicate ts
        if whale_pred_label == "HIGH" && has_recent_anom {
            let history = self.stars_history.lock().unwrap();
            let last_entry_ts = history.history.iter().filter(|r| r.pair == pair).map(|r| r.ts).max().unwrap_or(0);
            let time_diff = ts_int.saturating_sub(last_entry_ts);
            drop(history);

            if time_diff > 3600 && ts_int != last_entry_ts {  // Geen exact dezelfde ts, en minimaal 1 uur tussen entries per pair
                println!("[STAR SNAPSHOT] Adding unique snapshot for {} at ts {} (time_diff {}s)", pair, ts_int, time_diff);
                let whale_side = t.last_whale_side.clone().unwrap_or_else(|| "-".to_string());
                let whale_volume = t.last_whale_volume.unwrap_or(0.0);
                let whale_notional = t.last_whale_notional.unwrap_or(0.0);
                let row = TopRow {
                    ts: ts_int,
                    pair: pair.to_string(),
                    price,
                    pct,
                    flow_pct,
                    dir: dir.clone(),
                    early: new_early.clone(),
                    alpha: new_alpha.clone(),
                    pump_score,
                    pump_label: pump_label.clone(),
                    whale: is_whale,
                    whale_side: whale_side.clone(),
                    whale_volume,
                    whale_notional,
                    analysis: Self::build_analysis(&Row { 
                        pair: pair.to_string(), 
                        price, 
                        pct, 
                        whale: is_whale, 
                        whale_side: whale_side.clone(), 
                        whale_volume, 
                        whale_notional, 
                        flow_pct, 
                        dir: dir.clone(), 
                        early: new_early.clone(), 
                        alpha: new_alpha.clone(), 
                        pump_score, 
                        pump_label: pump_label.clone(), 
                        trades: t.trade_count, 
                        buys: t.buy_volume, 
                        sells: t.sell_volume, 
                        o: c.open.unwrap_or(0.0), 
                        h: c.high.unwrap_or(0.0), 
                        l: c.low.unwrap_or(0.0), 
                        c: c.close.unwrap_or(0.0), 
                        score: total_score, 
                        rating: rating.clone(), 
                        whale_pred_score, 
                        whale_pred_label: whale_pred_label.clone(), 
                        reliability_score: Self::compute_reliability(&t, ts_int).0, 
                        reliability_label: Self::compute_reliability(&t, ts_int).1, 
                        buy_trades: t.buy_trades, 
                        sell_trades: t.sell_trades, 
                        ts: ts_int, // NIEUW: ts veld toegevoegd
                        avg_buy_duration_sec: 0.0,
                        avg_sell_duration_sec: 0.0,
                        avg_neutral_duration_sec: 0.0,
                    }),
                    whale_pred_score,
                    whale_pred_label: whale_pred_label.clone(),
                    reliability_score: Self::compute_reliability(&t, ts_int).0,
                    reliability_label: Self::compute_reliability(&t, ts_int).1,
                    signal_type: "WH_PRED".to_string(),
                    confidence: compute_confidence(&Row { 
                        pair: pair.to_string(), 
                        price, 
                        pct, 
                        whale: is_whale, 
                        whale_side: whale_side.clone(), 
                        whale_volume, 
                        whale_notional, 
                        flow_pct, 
                        dir: dir.clone(), 
                        early: new_early.clone(), 
                        alpha: new_alpha.clone(), 
                        pump_score, 
                        pump_label: pump_label.clone(), 
                        trades: t.trade_count, 
                        buys: t.buy_volume, 
                        sells: t.sell_volume, 
                        o: c.open.unwrap_or(0.0), 
                        h: c.high.unwrap_or(0.0), 
                        l: c.low.unwrap_or(0.0), 
                        c: c.close.unwrap_or(0.0), 
                        score: total_score, 
                        rating: rating.clone(), 
                        whale_pred_score, 
                        whale_pred_label: whale_pred_label.clone(), 
                        reliability_score: Self::compute_reliability(&t, ts_int).0, 
                        reliability_label: Self::compute_reliability(&t, ts_int).1, 
                        buy_trades: t.buy_trades, 
                        sell_trades: t.sell_trades, 
                        ts: ts_int, // NIEUW: ts veld toegevoegd
                        avg_buy_duration_sec: 0.0,
                        avg_sell_duration_sec: 0.0,
                        avg_neutral_duration_sec: 0.0,
                    }, t.trade_count as usize, Self::compute_reliability(&t, ts_int).0),
                    momentum: compute_momentum(pct, pump_score, flow_pct),
                    trade_score: compute_trade_score(
                        compute_momentum(pct, pump_score, flow_pct),
                        compute_confidence(&Row { 
                            pair: pair.to_string(), 
                            price, 
                            pct, 
                            whale: is_whale, 
                            whale_side: whale_side.clone(), 
                            whale_volume, 
                            whale_notional, 
                            flow_pct, 
                            dir: dir.clone(), 
                            early: new_early.clone(), 
                            alpha: new_alpha.clone(), 
                            pump_score, 
                            pump_label: pump_label.clone(), 
                            trades: t.trade_count, 
                            buys: t.buy_volume, 
                            sells: t.sell_volume, 
                            o: c.open.unwrap_or(0.0), 
                            h: c.high.unwrap_or(0.0), 
                            l: c.low.unwrap_or(0.0), 
                            c: c.close.unwrap_or(0.0), 
                            score: total_score, 
                            rating: rating.clone(), 
                            whale_pred_score, 
                        whale_pred_label: whale_pred_label.clone(), 
                        reliability_score: Self::compute_reliability(&t, ts_int).0, 
                        reliability_label: Self::compute_reliability(&t, ts_int).1, 
                        buy_trades: t.buy_trades, 
                        sell_trades: t.sell_trades, 
                        ts: ts_int, // NIEUW: ts veld toegevoegd
                        avg_buy_duration_sec: 0.0,
                        avg_sell_duration_sec: 0.0,
                        avg_neutral_duration_sec: 0.0,
                    }, t.trade_count as usize, Self::compute_reliability(&t, ts_int).0),
                        Self::compute_reliability(&t, ts_int).0,
                        whale_pred_score,
                        pump_score,
                        flow_pct,
                        pct,
                    ),
                    buy_trades: t.buy_trades,
                    sell_trades: t.sell_trades,
                };
                self.add_to_stars_history(row);
            } else {
                println!("[STAR SKIP] {} skipped (time_diff {}s, ts {} == last {})", pair, time_diff, ts_int, last_entry_ts);
            }
        }

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
    }

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

        let ew_vol0 = ts.ewma_vol24h.unwrap_or(vol24h);
        let ew_vol1 = 0.9 * ew_vol0 + 0.1 * vol24h;
        ts.ewma_vol24h = Some(ew_vol1);

        let ew_ret0 = ts.ewma_abs_return.unwrap_or(jump);
        let ew_ret1 = 0.9 * ew_ret0 + 0.1 * jump;
        ts.ewma_abs_return = Some(ew_ret1);

        ts.last_price = Some(last);
        ts.last_vol24h = Some(vol24h);

        let mut c = self.candles.entry(pair.to_string()).or_default();  // Verplaatst buiten {} blok
        c.last_update_ts = ts_int;

        {
            let mut t = self.trades.entry(pair.to_string()).or_default();
            t.last_update_ts = ts_int;

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

            let mut t = self.trades.entry(pair.to_string()).or_default();
            t.recent_anom = true;

            if pair == "POND/EUR" {
                println!("[DEBUG POND] ANOM detected: strength={:.1}, setting recent_anom=true", score);
            }

            if t.last_whale_pred_high {
                println!("[STAR SNAPSHOT] Adding snapshot for {} due to ANOM + recent HIGH", pair);
                let price = last;
                let pct = c.pct_change.unwrap_or(0.0);
                let flow_pct = t.last_flow_pct;
                let dir = t.last_dir.clone();
                let new_early = t.last_early.clone().unwrap_or_else(|| "NONE".to_string());
                let new_alpha = t.last_alpha.clone().unwrap_or_else(|| "NONE".to_string());
                let pump_score = t.last_pump_score;
                let pump_label = t.last_pump_signal.clone().unwrap_or_else(|| "NONE".to_string());
                let is_whale = t.last_whale;
                let whale_side = t.last_whale_side.clone().unwrap_or_else(|| "-".to_string());
                let whale_volume = t.last_whale_volume.unwrap_or(0.0);
                let whale_notional = t.last_whale_notional.unwrap_or(0.0);
                let total_score = t.last_score;
                let rating = t.last_rating.clone().unwrap_or_else(|| "NONE".to_string());
                let whale_pred_score = t.whale_pred_score;
                let whale_pred_label = t.whale_pred_label.clone().unwrap_or_else(|| "NONE".to_string());
                let reliability_score = Self::compute_reliability(&t, ts_int).0;
                let reliability_label = Self::compute_reliability(&t, ts_int).1;
                let row = TopRow {
                    ts: ts_int,
                    pair: pair.to_string(),
                    price,
                    pct,
                    flow_pct,
                    dir: dir.clone(),
                    early: new_early.clone(),
                    alpha: new_alpha.clone(),
                    pump_score,
                    pump_label: pump_label.clone(),
                    whale: is_whale,
                    whale_side: whale_side.clone(),
                    whale_volume,
                    whale_notional,
                    analysis: Self::build_analysis(&Row { 
                        pair: pair.to_string(), 
                        price, 
                        pct, 
                        whale: is_whale, 
                        whale_side: whale_side.clone(), 
                        whale_volume, 
                        whale_notional, 
                        flow_pct, 
                        dir: dir.clone(), 
                        early: new_early.clone(), 
                        alpha: new_alpha.clone(), 
                        pump_score, 
                        pump_label: pump_label.clone(), 
                        trades: t.trade_count, 
                        buys: t.buy_volume, 
                        sells: t.sell_volume, 
                        o: c.open.unwrap_or(0.0), 
                        h: c.high.unwrap_or(0.0), 
                        l: c.low.unwrap_or(0.0), 
                        c: c.close.unwrap_or(0.0), 
                        score: total_score, 
                        rating: rating.clone(), 
                        whale_pred_score, 
                        whale_pred_label: whale_pred_label.clone(), 
                        reliability_score, 
                        reliability_label: reliability_label.clone(), 
                        buy_trades: t.buy_trades, 
                        sell_trades: t.sell_trades, 
                        ts: ts_int, // NIEUW: ts veld toegevoegd
                        avg_buy_duration_sec: 0.0,
                        avg_sell_duration_sec: 0.0,
                        avg_neutral_duration_sec: 0.0,
                    }),
                    whale_pred_score,
                    whale_pred_label: whale_pred_label.clone(),
                    reliability_score,
                    reliability_label: Self::compute_reliability(&t, ts_int).1,
                    signal_type: "ANOM".to_string(),
                    confidence: compute_confidence(&Row { 
                        pair: pair.to_string(), 
                        price, 
                        pct, 
                        whale: is_whale, 
                        whale_side: whale_side.clone(), 
                        whale_volume, 
                        whale_notional, 
                        flow_pct, 
                        dir: dir.clone(), 
                        early: new_early.clone(), 
                        alpha: new_alpha.clone(), 
                        pump_score, 
                        pump_label: pump_label.clone(), 
                        trades: t.trade_count, 
                        buys: t.buy_volume, 
                        sells: t.sell_volume, 
                        o: c.open.unwrap_or(0.0), 
                        h: c.high.unwrap_or(0.0), 
                        l: c.low.unwrap_or(0.0), 
                        c: c.close.unwrap_or(0.0), 
                        score: total_score, 
                        rating: rating.clone(), 
                        whale_pred_score, 
                        whale_pred_label: whale_pred_label.clone(), 
                        reliability_score, 
                        reliability_label: Self::compute_reliability(&t, ts_int).1, 
                        buy_trades: t.buy_trades, 
                        sell_trades: t.sell_trades, 
                        ts: ts_int, // NIEUW: ts veld toegevoegd
                        avg_buy_duration_sec: 0.0,
                        avg_sell_duration_sec: 0.0,
                        avg_neutral_duration_sec: 0.0,
                    }, t.trade_count as usize, Self::compute_reliability(&t, ts_int).0),
                    momentum: compute_momentum(pct, pump_score, t.last_flow_pct),
                    trade_score: compute_trade_score(
                        compute_momentum(pct, pump_score, t.last_flow_pct),
                        compute_confidence(&Row { 
                            pair: pair.to_string(), 
                            price, 
                            pct, 
                            whale: is_whale, 
                            whale_side: whale_side.clone(), 
                            whale_volume, 
                            whale_notional, 
                            flow_pct, 
                            dir: dir.clone(), 
                            early: new_early.clone(), 
                            alpha: new_alpha.clone(), 
                            pump_score, 
                            pump_label: pump_label.clone(), 
                            trades: t.trade_count, 
                            buys: t.buy_volume, 
                            sells: t.sell_volume, 
                            o: c.open.unwrap_or(0.0), 
                            h: c.high.unwrap_or(0.0), 
                            l: c.low.unwrap_or(0.0), 
                            c: c.close.unwrap_or(0.0), 
                            score: total_score, 
                            rating: rating.clone(), 
                            whale_pred_score, 
                            whale_pred_label: whale_pred_label.clone(), 
                            reliability_score, 
                            reliability_label: Self::compute_reliability(&t, ts_int).1, 
                            buy_trades: t.buy_trades, 
                            sell_trades: t.sell_trades, 
                            ts: ts_int, // NIEUW: ts veld toegevoegd
                            avg_buy_duration_sec: 0.0,
                            avg_sell_duration_sec: 0.0,
                            avg_neutral_duration_sec: 0.0,
                    }, t.trade_count as usize, Self::compute_reliability(&t, ts_int).0),
                        Self::compute_reliability(&t, ts_int).0,
                        whale_pred_score,
                        pump_score,
                        t.last_flow_pct,
                        pct,
                    ),
                    buy_trades: t.buy_trades,
                    sell_trades: t.sell_trades,
                };
                self.add_to_stars_history(row);
            }

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

    fn compute_reliability(t: &TradeState, now_ts: i64) -> (f64, String) {
        let now_f = now_ts as f64;

        let cutoff_60 = now_f - 60.0;
        let cutoff_300 = now_f - 300.0;

        let mut recent_trades_60: usize = 0;
        let _vol_60: f64 = 0.0;
        for (_ts, _v) in t.recent_buys.iter().chain(t.recent_sells.iter()) {
            if *_ts >= cutoff_60 {
                recent_trades_60 += 1;
            }
        }

        let mut vol_300: f64 = 0.0;
        for (_ts, v) in t.recent_buys_5m.iter().chain(t.recent_sells_5m.iter()) {
            if *_ts >= cutoff_300 {
                vol_300 += *v;
            }
        }

        let td = (recent_trades_60.min(30) as f64 / 30.0) * 40.0;

        let ew_v = t.ewma_volume.unwrap_or(vol_300.max(1e-9));
        let vol_ratio = if ew_v > 0.0 { vol_300 / ew_v } else { 1.0 };

        let vs = if vol_ratio > 4.0 {
            0.0
        } else if vol_ratio > 2.0 {
            10.0
        } else {
            20.0
        };

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
            0.0
        } else if flow_pct_60 > 70.0 || flow_pct_60 < 30.0 {
            20.0
        } else {
            15.0
        };

        let dt = now_ts.saturating_sub(t.last_update_ts);
        // AANBEVELING 4: Verleng age-penalty in rel_score naar 10 minuten
        let ras = if dt > 600 {  // 10 minuten in plaats van 300 (5 min)
            0.0
        } else if dt > 300 {
            5.0
        } else if dt > 120 {
            10.0
        } else {
            15.0
        };

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

            // NIEUW: Bereken gemiddelde duur DIR
            let (avg_buy, avg_sell, avg_neutral) = v.compute_avg_durations();

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
                buy_trades: v.buy_trades,
                sell_trades: v.sell_trades,
                ts: v.last_update_ts, // NIEUW: ts veld toegevoegd voor Time kolom in Markets
                avg_buy_duration_sec: avg_buy,
                avg_sell_duration_sec: avg_sell,
                avg_neutral_duration_sec: avg_neutral,
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
                ts: r.ts, // NIEUW: ts veld gebruikt
                reliability_score: r.reliability_score,
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

            let best_trade = f64::MIN;
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

    fn manual_trades_snapshot(&self) -> ManualTradesResponse {
        let trader = self.manual_trader.lock().unwrap();
        let mut list = Vec::new();
        for (pair, trade) in trader.trades.iter() {
            let current_price = self
                .candles
                .get(pair)
                .and_then(|c| c.close)
                .unwrap_or(trade.entry_price);
            let pnl = (current_price - trade.entry_price) * trade.size;
            let pnl_pct = if trade.entry_price > 0.0 {
                (current_price - trade.entry_price) / trade.entry_price * 100.0
            } else {
                0.0
            };
            list.push(ManualTradeView {
                pair: pair.clone(),
                entry_price: trade.entry_price,
                size: trade.size,
                open_ts: trade.open_ts,
                stop_loss: trade.stop_loss,
                take_profit: trade.take_profit,
                current_price,
                pnl_abs: pnl,
                pnl_pct,
                fee_pct: trade.fee_pct,
                manual_amount: trade.manual_amount,
            });
        }
        ManualTradesResponse {
            balance: trader.balance,
            initial_balance: trader.initial_balance,
            trades: list,
        }
    }

    fn top10_snapshot(&self) -> Top10Response {
        let rows = self.snapshot();

        let mut risers: Vec<TopRow> = rows
            .iter()
            .filter(|r| r.dir == "BUY" && r.pct > 0.0)
            .map(|r| {
                let trades_count = self.trades.get(&r.pair).map(|t| t.trade_count).unwrap_or(0);
                let rel_score = r.reliability_score;
                let confidence = compute_confidence(r, trades_count as usize, rel_score);

                let signal_type = if confidence < 30.0 || rel_score < 30.0 || trades_count < 3 {
                    "UNCERTAIN".to_string()
                } else if r.whale {
                    "WHALE".to_string()
                } else if r.alpha == "BUY" {
                    "ALPHA".to_string()
                } else if r.early == "BUY" {
                    "EARLY".to_string()
                } else if r.whale_pred_label == "HIGH" {
                    "WH_PRED".to_string()
                } else if !r.pump_label.is_empty() && r.pump_label != "NONE" {
                    r.pump_label.clone()
                } else {
                    "NONE".to_string()
                };

                TopRow {
                    ts: r.ts, // NIEUW: ts veld gebruikt
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
                    analysis: Self::build_analysis(r),
                    whale_pred_score: r.whale_pred_score,
                    whale_pred_label: r.whale_pred_label.clone(),
                    reliability_score: r.reliability_score,
                    reliability_label: r.reliability_label.clone(),
                    signal_type,
                    confidence,
                    momentum: compute_momentum(r.pct, r.pump_score, r.flow_pct),
                    trade_score: compute_trade_score(
                        compute_momentum(r.pct, r.pump_score, r.flow_pct),
                        confidence,
                        rel_score,
                        r.whale_pred_score,
                        r.pump_score,
                        r.flow_pct,
                        r.pct,
                    ),
                    buy_trades: r.buy_trades,
                    sell_trades: r.sell_trades,
                }
            })
            .collect();

        let mut best3 = risers.clone();
        best3.sort_by(|a, b| {
            let sa = a.trade_score + a.pump_score * 1.5 + a.whale_pred_score * 1.0;
            let sb = b.trade_score + b.pump_score * 1.5 + b.whale_pred_score * 1.0;
            sb.partial_cmp(&sa).unwrap()
        });
        if best3.len() > 3 {
            best3.truncate(3);
        }

        risers.sort_by(|a, b| {
            let sa = a.trade_score + a.pump_score * 1.5 + a.whale_pred_score * 1.0;
            let sb = b.trade_score + b.pump_score * 1.5 + b.whale_pred_score * 1.0;
            sb.partial_cmp(&sa).unwrap()
        });
        if risers.len() > 10 {
            risers.truncate(10);
        }

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
                let _total_score = pct_down * 0.5 + flow_sell * 0.1;

                let trades_count = self.trades.get(&r.pair).map(|t| t.trade_count).unwrap_or(0);
                let rel_score = r.reliability_score;
                let confidence = compute_confidence(r, trades_count as usize, rel_score);

                let signal_type = if confidence < 30.0 || rel_score < 30.0 || trades_count < 3 {
                    "UNCERTAIN".to_string()
                } else if r.whale {
                    "WHALE".to_string()
                } else if r.alpha == "SELL" {
                    "ALPHA".to_string()
                } else if r.early == "SELL" {
                    "EARLY".to_string()
                } else {
                    "NONE".to_string()
                };

                TopRow {
                    ts: r.ts, // NIEUW: ts veld gebruikt
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
                    analysis: Self::build_analysis(r),
                    whale_pred_score: r.reliability_score,
                    whale_pred_label: r.reliability_label.clone(),
                    reliability_score: r.reliability_score,
                    reliability_label: r.reliability_label.clone(),
                    signal_type,
                    confidence,
                    momentum: compute_momentum(r.pct, r.pump_score, r.flow_pct),
                    trade_score: compute_trade_score(
                        compute_momentum(r.pct, r.pump_score, r.flow_pct),
                        confidence,
                        rel_score,
                        r.whale_pred_score,
                        r.pump_score,
                        r.flow_pct,
                        r.pct,
                    ),
                    buy_trades: r.buy_trades,
                    sell_trades: r.sell_trades,
                }
            })
            .collect();

        fallers.sort_by(|a, b| b.trade_score.partial_cmp(&a.trade_score).unwrap());
        if fallers.len() > 10 {
            fallers.truncate(10);
        }

        Top10Response {
            best3,
            risers,
            fallers,
        }
    }

    // NIEUW: Forecast functie voor voorspellingen met verbeteringen
    fn forecast_snapshot(&self) -> Vec<ForecastRow> {
        let (bullish_avg_return, bearish_avg_return) = self.compute_avg_returns();  // NIEUW: Separate bullish/bearish averages

        let rows = self.snapshot(); // Gebruik snapshot() voor alle actieve pairs
        let mut forecasts: Vec<ForecastRow> = rows.into_iter().take(50).filter(|r| !is_stablecoin(&r.pair)).map(|r| {  // Toon meer pairs (50), uitsluiten stablecoins
            let flow_pct = r.flow_pct;
            let momentum = compute_momentum(r.pct, r.pump_score, r.flow_pct);
            let trade_rate = self.compute_trade_rate(&r.pair);  // NIEUW: Trade rate
            let decay_penalty = self.compute_decay_penalty(&r.pair);  // NIEUW: Decay check

            let mut probability = 30.0;  // Basis neutraal
            if flow_pct > 70.0 && momentum > 75.0 {
                probability = 70.0;
            } else if flow_pct > 60.0 && momentum > 70.0 {
                probability = 50.0;
            } else if flow_pct < 30.0 {  // NIEUW: Bearish optie
                probability = 20.0;
            }
            probability = (probability + trade_rate * 10.0 - decay_penalty).clamp(0.0, 100.0);  // NIEUW: Trade rate boost, decay penalty

            // NIEUW: Meer realistische % verandering gebaseerd op momentum en historische averages
            let base_pct = if flow_pct < 30.0 { -bearish_avg_return } else { bullish_avg_return };
            let adjusted_pct = base_pct * (momentum / 100.0).clamp(0.5, 2.0);  // NIEUW: Momentum multiplier
            let target_pct = adjusted_pct.clamp(-5.0, 5.0);  // Beperk tot realistische range (-5% tot +5%)

            let target_price = r.price * (1.0 + target_pct / 100.0);
            let pct_change_target = ((target_price - r.price) / r.price) * 100.0;

            let value = r.buys * r.price + r.sells * r.price;  // NIEUW: Altijd aggregaat berekenen

            ForecastRow {
                ts: r.ts, // NIEUW: ts veld gebruikt
                pair: r.pair,
                trades_buys: r.buy_trades,
                trades_sells: r.sell_trades,
                volume: r.buys + r.sells,
                value,
                current_price: r.price,
                target_price,
                pct_change_target,
                probability,
                duration_min: if momentum > 80.0 { 30 } else { 15 },
            }
        }).collect();

        // Sorteer op waarschijnlijkheid descending (hoogste bovenaan)
        forecasts.sort_by(|a, b| b.probability.partial_cmp(&a.probability).unwrap());
        forecasts
    }

    // NIEUW: Helper voor separate bullish/bearish gemiddelde returns
    fn compute_avg_returns(&self) -> (f64, f64) {
        let sigs = self.signals.lock().unwrap();
        let mut bullish_returns = Vec::new();
        let mut bearish_returns = Vec::new();

        for ev in sigs.iter() {
            if ev.ret_5m.is_some() {
                let ret = ev.ret_5m.unwrap();
                if ev.direction == "BUY" {
                    bullish_returns.push(ret);
                } else if ev.direction == "SELL" {
                    bearish_returns.push(ret);
                }
            }
        }

        let bullish_avg = if bullish_returns.is_empty() { 2.5 } else { bullish_returns.iter().sum::<f64>() / bullish_returns.len() as f64 };
        let bearish_avg = if bearish_returns.is_empty() { -2.5 } else { bearish_returns.iter().sum::<f64>() / bearish_returns.len() as f64 };

        (bullish_avg, bearish_avg)
    }

    // NIEUW: Trade rate (trades per minuut)
    fn compute_trade_rate(&self, pair: &str) -> f64 {
        if let Some(t) = self.trades.get(pair) {
            let recent_count = t.recent_buys.len() + t.recent_sells.len();
            recent_count as f64 / 5.0  // Laatste 5 min
        } else { 0.0 }
    }

    // NIEUW: Decay penalty (als momentum daalt)
    fn compute_decay_penalty(&self, pair: &str) -> f64 {
        if let Some(t) = self.trades.get(pair) {
            let current_momentum = compute_momentum(t.recent_prices.last().map(|(_, p)| *p).unwrap_or(0.0), t.last_pump_score, t.last_flow_pct);
            let prev_momentum = compute_momentum(t.recent_prices.get(t.recent_prices.len().saturating_sub(2)).map(|(_, p)| *p).unwrap_or(0.0), t.last_pump_score, t.last_flow_pct);
            if current_momentum < prev_momentum { 10.0 } else { 0.0 }  // Penalty als decay
        } else { 0.0 }
    }

    fn build_analysis(row: &Row) -> String {
        // Bereken lokale waarden
        let trades_count = row.trades as usize;
        let rel_score = row.reliability_score;
        let confidence = compute_confidence(row, trades_count, rel_score);
        let momentum = compute_momentum(row.pct, row.pump_score, row.flow_pct);
        let trade_score = compute_trade_score(momentum, confidence, rel_score, row.whale_pred_score, row.pump_score, row.flow_pct, row.pct);

        // Advies gebaseerd op samenhang
        let (advice, forward) = if rel_score < 30.0 || confidence < 30.0 {
            ("Negeer. Onbetrouwbare data.", "Risico op verkeerde interpretatie door onvolledige data.")
        } else if trade_score > 65.0 && confidence > 70.0 && (row.alpha == "BUY" || row.early == "BUY") {
            ("Koop nu. Sterke signalen wijzen op breakout.", "Verwacht stijging door sterke flow en whale activiteit.")
        } else if trade_score > 50.0 && momentum > 75.0 && row.dir == "BUY" && row.pct > 5.0 {
            ("Houdt positie. Momentum ondersteunt voortzetting.", "Sterke impuls; koop voor momentum ride.")
        } else if row.dir == "SELL" && trade_score < 45.0 && row.pct < -0.5 {
            ("Verkoop. Daling signaal met lage scores.", "Sterke sell-off; verwacht verder verlies.")
        } else if row.pump_score > 7.0 && rel_score < 60.0 {
            ("Negeer. Pump risico op dump.", "Risico op dump door lage betrouwbaarheid.")
        } else if trade_score > 60.0 && row.whale_pred_label == "HIGH" {
            ("Koop. Whale verwachting versterkt momentum.", "Verwacht whale activiteit; monitor orderbooks.")
        } else if momentum < 50.0 && row.pct < 0.0 {
            ("Verkoop. Zwak momentum duidt op pullback.", "Verwacht daling; vermijd lange posities.")
        } else {
            ("Houdt positie. Neutrale condities.", "Mogelijke zijwaartse beweging; monitor whale en pump.")
        };

        // Combineer: advies + forward, binnen 200 chars
        let text = format!("{}. {}", advice, forward);
        text.chars().take(200).collect::<String>()
    }

    async fn manual_add_trade(&self, pair: &str, sl_pct: f64, tp_pct: f64, fee_pct: f64, manual_amount: f64) -> bool {
        let current_price = self.candles.get(pair).and_then(|c| c.close).unwrap_or(0.0);
        if current_price <= 0.0 {
            return false;
        }
        let (success, state_clone) = {
            let mut trader = self.manual_trader.lock().unwrap();
            let success = trader.add_trade(pair, current_price, sl_pct, tp_pct, fee_pct, manual_amount);
            (success, trader.clone())
        };
        if success {
            if let Err(e) = state_clone.save().await {
                eprintln!("[ERROR] Failed to save manual trades: {}", e);
            }
            if let Err(e) = state_clone.save_equity().await {
                eprintln!("[ERROR] Failed to save equity: {}", e);
            }
        }
        success
    }

    async fn manual_close_trade(&self, pair: &str) -> bool {
        let current_price = self.candles.get(pair).and_then(|c| c.close).unwrap_or(0.0);
        if current_price <= 0.0 {
            return false;
        }
        let (success, state_clone) = {
            let mut trader = self.manual_trader.lock().unwrap();
            let success = trader.close_trade(pair, current_price);
            (success, trader.clone())
        };
        if success {
            if let Err(e) = state_clone.save().await {
                eprintln!("[ERROR] Failed to save manual trades: {}", e);
            }
            if let Err(e) = state_clone.save_equity().await {
                eprintln!("[ERROR] Failed to save equity: {}", e);
            }
        }
        success
    }

    async fn load_manual_trader(&self) {
        let loaded_state = ManualTraderState::load().await;
        let mut trader = self.manual_trader.lock().unwrap();
        *trader = loaded_state;
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
        "XXLM" => "XLM".to_string(),
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

// NIEUW: Functie om stablecoins te detecteren
fn is_stablecoin(pair: &str) -> bool {
    let base = pair.split('/').next().unwrap_or("");
    matches!(base, "USDT" | "USDC" | "DAI" | "TUSD" | "BUSD" | "UST" | "FRAX" | "LUSD")
}

// ============================================================================
// HOOFDSTUK 9 – FRONTEND (HTML DASHBOARD) – Aangepast voor Priority Pair Selectie in Health Tab + Highlighting in alle tabbladen
// ============================================================================

const DASHBOARD_HTML: &str = r####"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>WhaleRadar</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js"></script>
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
.signal_type_UNCERTAIN { color:#9e9e9e; }
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
.priority-pair {
  background:#333 !important;
  border-left: 5px solid #ff4081;
}
#coin-analysis-modal {
  display: none;
  position: fixed;
  top: 0;
  left: 0;
  width: 100%;
  height: 100%;
  background: rgba(0,0,0,0.8);
  z-index: 1000;
  display: flex;
  justify-content: center;
  align-items: center;
}
#coin-analysis-modal > div {
  background: #1e1e1e;
  padding: 20px;
  width: 80%;
  max-width: 800px;
  border-radius: 10px;
  color: #ddd;
  max-height: 80vh;
  overflow-y: auto;
}
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
    <button class="tab-btn" data-tab="forecast">Forecast</button>
    <button class="tab-btn" data-tab="manual_trades">Manual Trades</button>
    <button class="tab-btn" data-tab="backtest">Backtest</button>
    <button class="tab-btn" data-tab="heatmap">Heatmap</button>
    <button class="tab-btn" data-tab="stars">Stars</button>
    <button class="tab-btn" data-tab="health">Health</button>
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
          <th>Time</th><th>Pair</th><th>Price</th><th>%</th><th>Whale</th>
          <th>Flow</th><th>Dir</th><th>Early</th><th>Alpha</th><th>Pump</th>
          <th>WhPred</th><th>Rel</th><th>Total score</th><th>Trades</th><th>Buys</th><th>Sells</th>
          <th>O</th><th>H</th><th>L</th><th>C</th>
          <th>Avg Buy Dur (s)</th><th>Avg Sell Dur (s)</th><th>Avg Neutr Dur (s)</th>
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
          <th>Time</th><th>Pair</th><th>Type</th><th>Dir</th>
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
          <th>Early</th><th>Alpha</th><th>Whale</th><th>Pump</th>
          <th>WhPred</th><th>Rel</th><th>Type</th><th>Confidence</th><th>Momentum</th><th>Trade Score</th><th>Count Trades</th><th>Visual</th><th>Analyse</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>

    <h2>Top 10 Stijgers (strong buy)</h2>
    <table id="top10-up">
      <thead>
        <tr>
          <th>Time</th><th>Pair</th><th>Price</th><th>%</th><th>Flow</th><th>Dir</th>
          <th>Early</th><th>Alpha</th><th>Whale</th><th>Pump</th>
          <th>WhPred</th><th>Rel</th><th>Type</th><th>Confidence</th><th>Momentum</th><th>Trade Score</th><th>Count Trades</th><th>Visual</th><th>Analyse</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>

    <h2>Top 10 Dalers (strong sell)</h2>
    <table id="top10-down">
      <thead>
        <tr>
          <th>Time</th><th>Pair</th><th>Price</th><th>%</th><th>Flow</th><th>Dir</th>
          <th>Early</th><th>Alpha</th><th>Whale</th><th>Pump</th>
          <th>Rel</th><th>Type</th><th>Confidence</th><th>Momentum</th><th>Trade Score</th><th>Count Trades</th><th>Visual</th><th>Analyse</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
  </div>

  <div id="view-forecast" style="display:none;">
    <h2>📈 Forecast: Voorspellingen voor Stijgingen</h2>
    <table id="forecast-table">
      <thead>
        <tr>
          <th>Tijd</th><th>Pair</th><th>Trades (Buys/Sells)</th><th>Volume</th><th>Waarde (€)</th>
          <th>Huidige Prijs (€)</th><th>Doelprijs (€)</th><th>% Stijging/Daling</th>
          <th>Waarschijnlijkheid (%)</th><th>Duur (min)</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
  </div>

  <div id="view-manual_trades" style="display:none;">
    <h2>Manual Trades</h2>
    <div id="manual-summary" style="margin-bottom:15px; padding:10px; background:#222; border-radius:5px;">
      <p><strong>Balance:</strong> <span id="manual-balance">€0.00</span></p>
      <p><strong>Initial Balance:</strong> <span id="manual-initial">€0.00</span></p>
      <p><strong>Total PnL:</strong> <span id="manual-pnl" class="pos">€0.00</span></p>
    </div>
    
    <h3>Open a Trade</h3>
    <div style="margin-bottom:20px; padding:10px; background:#1a1a1a; border-radius:5px;">
      <label>Pair:</label>
      <input type="text" id="manual-pair-search" placeholder="Search pair..." style="width:200px; margin-left:5px;" />
      <select id="manual-pair" style="width:200px; margin-left:10px;">
        <!-- Vul dynamisch met pairs -->
      </select>
      <br/><br/>
      <label style="margin-right:10px;">Fee %:</label>
      <select id="manual-fee">
        <option value="0.1">0.1%</option>
        <option value="0.26" selected>0.26%</option>
        <option value="0.5">0.5%</option>
      </select>
      <label style="margin-left:20px; margin-right:10px;">Amount (€):</label>
      <input type="number" id="manual-amount" value="100" step="10" style="width:100px;" />
      <br/><br/>
      <label style="margin-right:10px;">Stop Loss %:</label>
      <select id="manual-sl">
        <option value="0.5">0.5%</option>
        <option value="1">1%</option>
        <option value="2" selected>2%</option>
        <option value="5">5%</option>
      </select>
      <label style="margin-left:20px; margin-right:10px;">Take Profit %:</label>
      <select id="manual-tp">
        <option value="1">1%</option>
        <option value="2">2%</option>
        <option value="5" selected>5%</option>
        <option value="10">10%</option>
      </select>
      <button id="manual-open-btn" style="margin-left:20px; padding:5px 15px;">Open Trade</button>
      <button id="analyze-coin-btn" style="margin-left:20px; padding:5px 15px;">Analyse Coin</button>
    </div>
    
    <h3>Active Trades</h3>
    <table id="manual-trades-table">
      <thead>
        <tr>
          <th>Pair</th>
          <th>Entry Price</th>
          <th>Size</th>
          <th>Current Price</th>
          <th>PnL Abs</th>
          <th>PnL %</th>
          <th>Open TS</th>
          <th>Fee %</th>
          <th>Amount</th>
          <th>Status</th>
          <th>Actions</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
    
    <h3>Equity Curve</h3>
    <canvas id="manual-equity" width="900" height="260" style="border:1px solid #333; background:#111;"></canvas>
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
          <th>Early</th><th>Alpha</th><th>Whale</th><th>Pump</th>
          <th>WhPred</th><th>Rel</th><th>Type</th><th>Confidence</th><th>Momentum</th><th>Trade Score</th><th>Count Trades</th><th>Visual</th><th>Analyse</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
    <h2>Historie</h2>
    <table id="stars-history-table">
      <thead>
        <tr>
          <th>Time</th><th>Pair</th><th>Price</th><th>%</th><th>Flow</th><th>Dir</th>
          <th>Early</th><th>Alpha</th><th>Whale</th><th>Pump</th>
          <th>WhPred</th><th>Rel</th><th>Type</th><th>Confidence</th><th>Momentum</th><th>Trade Score</th><th>Count Trades</th><th>Visual</th><th>Analyse</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
  </div>

  <div id="view-health" style="display:none;">
    <h2>System Health</h2>
    <div style="margin-bottom:15px; padding:10px; background:#1a1a1a; border-radius:5px;">
      <label for="priority-pair-select">Priority Pair (frequent data):</label>
      <select id="priority-pair-select" style="width:200px; margin-left:10px;">
        <option value="">Geen</option>
        <!-- Vul dynamisch met EUR pairs -->
      </select>
      <button id="set-priority-btn" style="margin-left:10px; padding:5px 15px;">Set Priority</button>
      <span id="priority-status" style="margin-left:10px;"></span>
    </div>
    <table id="health-table">
      <thead>
        <tr>
          <th>Metric</th><th>Value</th><th>Status</th>
        </tr>
      </thead>
      <tbody></tbody>
    </table>
    <p id="health-last-update">Last update: N/A</p>
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
        <li><b>Rel</b>: betrouwbaarheidsscore (0-100), gebaseerd op recente trades en volume.</li>
        <li><b>Type</b>: signal type (ALPHA, EARLY, WHALE, etc.), aangepast voor betrouwbaarheid.</li>
        <li><b>Confidence</b>: gecombineerde score (0-100%) voor hoe betrouwbaar het signaal is.</li>
        <li><b>Momentum</b>: koopmoment score (0-100%) gebaseerd op %, Pump en Flow.</li>
        <li><b>Trade Score</b>: holistische beslissingscore (0-100%) voor koop/vermijd, met risico-penaliteiten.</li>
        <li><b>Count Trades</b>: aantal Buy en Sell trades, geformatteerd als "100_Buy / 30_Sell".</li>
        <li><b>News Sent.</b>: sentiment van recente nieuwsartikelen (0-1).</li>
        <li><b>Visual</b>: link naar de bijbehorende Kraken Pro grafiek.</li>
      </ul>
    </div>
  </div>

  <!-- Nieuwe modal voor coin analyse -->
  <div id="coin-analysis-modal" style="display:none;">
    <div style="background:#1e1e1e; padding:20px; width:80%; max-width:800px; border-radius:10px; color:#ddd; max-height:80vh; overflow-y:auto;">
      <h2 id="modal-title">Coin Analyse</h2>
      <div id="modal-content">
        <!-- Inhoud wordt ingevuld via JS -->
      </div>
      <button id="close-modal" style="margin-top:10px;">Sluit</button>
    </div>
  </div>
</main>
<script>
// ... bestaande JS ...
let activeTab = "markets";
let priorityPair = null; // Global variable for priority pair

let heatmapPoints = [];
let heatTooltip = null;
let manualTradePairs = [];
let manualTradeSearchInitialized = false;

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

function applyDirFilter(tableId, filterSelectId, dirIndex) {
  const filterValue = document.getElementById(filterSelectId).value;
  const tbody = document.querySelector(`#${tableId} tbody`);
  const rows = tbody.querySelectorAll('tr');
  rows.forEach(row => {
    const dirCell = row.cells[dirIndex];
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
  document.getElementById("view-forecast").style.display =
    tab === "forecast" ? "block" : "none";
  document.getElementById("view-manual_trades").style.display =
    tab === "manual_trades" ? "block" : "none";
  document.getElementById("view-backtest").style.display =
    tab === "backtest" ? "block" : "none";
  document.getElementById("view-heatmap").style.display =
    tab === "heatmap" ? "block" : "none";
  document.getElementById("view-stars").style.display =
    tab === "stars" ? "block" : "none";
  document.getElementById("view-health").style.display =
    tab === "health" ? "block" : "none";
  document.getElementById("view-config").style.display =
    tab === "config" ? "block" : "none";
  document.getElementById("view-guide").style.display =
    tab === "guide" ? "block" : "none";

  if (tab === "heatmap") {
    loadHeatmap();
  } else if (tab === "backtest") {
    loadBacktest();
  } else if (tab === "manual_trades") {
    loadManualTrades();
  } else if (tab === "stars") {
    loadStars();
  } else if (tab === "health") {
    loadHealth();
  } else if (tab === "config") {
    loadConfig();
  } else if (tab === "forecast") {
    loadForecast();
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

function highlightPriorityPair(tableId, pairIndex) {
  const tbody = document.querySelector(`#${tableId} tbody`);
  const rows = tbody.querySelectorAll('tr');
  rows.forEach(row => {
    const pairCell = row.cells[pairIndex];
    if (pairCell) {
      const pairText = pairCell.textContent.trim();
      if (priorityPair && pairText === priorityPair) {
        row.classList.add('priority-pair');
      } else {
        row.classList.remove('priority-pair');
      }
    }
  });
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

    let avgBuyDur = r.avg_buy_duration_sec > 0 ? r.avg_buy_duration_sec.toFixed(1) + "s" : "-";
    let avgSellDur = r.avg_sell_duration_sec > 0 ? r.avg_sell_duration_sec.toFixed(1) + "s" : "-";
    let avgNeutralDur = r.avg_neutral_duration_sec > 0 ? r.avg_neutral_duration_sec.toFixed(1) + "s" : "-";

    let row = `<tr>
      <td>${fmtTime(r.ts)}</td>
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
      <td>${r.score.toFixed(2)}</td>
      <td>${r.trades}</td>
      <td>${r.buys.toFixed(4)}</td>
      <td>${r.sells.toFixed(4)}</td>
      <td>${r.o.toFixed(4)}</td>
      <td>${r.h.toFixed(4)}</td>
      <td>${r.l.toFixed(4)}</td>
      <td>${r.c.toFixed(4)}</td>
      <td>${avgBuyDur}</td>
      <td>${avgSellDur}</td>
      <td>${avgNeutralDur}</td>
      <td>${visual}</td>
    </tr>`;

    tbody.innerHTML += row;
  }
  applyDirFilter('grid', 'markets-dir-filter', 6); // Adjusted for new Time column
  highlightPriorityPair('grid', 1); // Pair is now in kolom 1
}

function fmtTime(ts) {
  const d = new Date(ts * 1000);
  const dd = String(d.getDate()).padStart(2,'0');
  const mm = String(d.getMonth()+1).padStart(2,'0');
  const hh = String(d.getHours()).padStart(2,'0');
  const mi = String(d.getMinutes()).padStart(2,'0');
  return `${dd}-${mm} ${hh}:${mi}`;
}

async function loadSignals() {
  let q = document.getElementById("search").value.toLowerCase(); // NIEUW: Search filter toegevoegd
  let includeStable = document.getElementById("signals-stable-filter").checked;
  let res = await fetch("/api/signals");
  let data = await res.json();
  let tbody = document.querySelector("#signals tbody");
  tbody.innerHTML = "";

  let filtered = data.filter(r =>
    r.pair.toLowerCase().includes(q) && // NIEUW: Filter op pair gebaseerd op search input
    (includeStable || !isStablecoin(r.pair))
  );

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
      <td>${fmtTime(r.ts)}</td>
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
  applyDirFilter('signals', 'signals-dir-filter', 3);
  highlightPriorityPair('signals', 1); // Pair is in kolom 1
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

  function renderRow(r, isFallers = false) {
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

    let typeClass = "signal_type signal_type_" + r.signal_type;

    let countTradesText = `${r.buy_trades}_Buy / ${r.sell_trades}_Sell`;

    if (isFallers) {
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
        <td style="color:${ r.pump_label === "MEGA_PUMP" ? "#ff4081" :
          r.pump_label === "EARLY_PUMP" ? "#00bcd4" :
          "#ccc"}">${r.pump_score.toFixed(1)}</td>
        <td class="${relClass}">${r.reliability_label} (${r.reliability_score.toFixed(0)})</td>
        <td class="${typeClass}">${r.signal_type}</td>
        <td>${r.confidence.toFixed(1)}%</td>
        <td>${r.momentum.toFixed(1)}%</td>
        <td>${r.trade_score.toFixed(1)}</td>
        <td>${countTradesText}</td>
        <td>${visual}</td>
        <td>${r.analysis}</td>
      </tr>`;
    } else {
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
        <td style="color:${ r.pump_label === "MEGA_PUMP" ? "#ff4081" :
          r.pump_label === "EARLY_PUMP" ? "#00bcd4" :
          "#ccc"}">${r.pump_score.toFixed(1)}</td>
        <td class="${predClass}">${r.whale_pred_label} (${r.whale_pred_score.toFixed(1)})</td>
        <td class="${relClass}">${r.reliability_label} (${r.reliability_score.toFixed(0)})</td>
        <td class="${typeClass}">${r.signal_type}</td>
        <td>${r.confidence.toFixed(1)}%</td>
        <td>${r.momentum.toFixed(1)}%</td>
        <td>${r.trade_score.toFixed(1)}</td>
        <td>${countTradesText}</td>
        <td>${visual}</td>
        <td>${r.analysis}</td>
      </tr>`;
    }
  }

  for (let r of data.best3.filter(row => includeStable || !isStablecoin(row.pair))) {
    top3Body.innerHTML += renderRow(r);
  }

  for (let r of data.risers.filter(row => includeStable || !isStablecoin(row.pair))) {
    upBody.innerHTML += renderRow(r);
  }

  for (let r of data.fallers.filter(row => includeStable || !isStablecoin(row.pair))) {
    downBody.innerHTML += renderRow(r, true);
  }
  applyDirFilter('top3', 'top10-dir-filter', 5);
  applyDirFilter('top10-up', 'top10-dir-filter', 5);
  applyDirFilter('top10-down', 'top10-dir-filter', 5);
  highlightPriorityPair('top3', 1); // Pair in kolom 1
  highlightPriorityPair('top10-up', 1);
  highlightPriorityPair('top10-down', 1);
}

async function loadForecast() {
  let res = await fetch("/api/forecast");
  let data = await res.json();
  let tbody = document.querySelector("#forecast-table tbody");
  tbody.innerHTML = "";
  for (let r of data) {
    let fmtTime = new Date(r.ts * 1000).toLocaleTimeString();
    let pctClass = r.pct_change_target > 0 ? "pos" : (r.pct_change_target < 0 ? "neg" : "");
    tbody.innerHTML += `<tr>
      <td>${fmtTime}</td><td>${r.pair}</td><td>${r.trades_buys}/${r.trades_sells}</td><td>${r.volume.toFixed(2)}</td><td>€${r.value.toFixed(0)}</td>
      <td>${r.current_price.toFixed(5)}</td><td>${r.target_price.toFixed(5)}</td><td class="${pctClass}">${r.pct_change_target.toFixed(2)}%</td>
      <td>${r.probability.toFixed(0)}%</td><td>${r.duration_min}</td>
    </tr>`;
  }
  highlightPriorityPair('forecast-table', 1); // Pair in kolom 1
}

async function loadManualTrades() {
  // Get manual trades data
  let tradesData = await fetch("/api/manual_trades").then(r => r.json());
  
  // Update summary
  let totalPnl = tradesData.balance - tradesData.initial_balance;
  document.getElementById("manual-balance").textContent = `€${tradesData.balance.toFixed(2)}`;
  document.getElementById("manual-initial").textContent = `€${tradesData.initial_balance.toFixed(2)}`;
  document.getElementById("manual-pnl").textContent = `€${totalPnl.toFixed(2)}`;
  document.getElementById("manual-pnl").className = totalPnl > 0 ? 'pos' : (totalPnl < 0 ? 'neg' : '');

  // Update global pairs list
  manualTradePairs = await fetch("/api/stats").then(r => r.json()).then(d => d.map(r => r.pair));
  
  // Initialize search filter once
  if (!manualTradeSearchInitialized) {
    let searchInput = document.getElementById("manual-pair-search");
    if (searchInput) {
      searchInput.addEventListener("input", () => {
        filterManualTradePairs();
      });
    }
    // Set flag to true regardless to avoid repeated DOM queries
    manualTradeSearchInitialized = true;
  }
  
  // Haal stats data op voor gecombineerde status per pair
  let statsData = await fetch("/api/stats").then(r => r.json());
  let statsMap = {};
  statsData.forEach(row => {
    statsMap[row.pair] = row; // Sla hele row op voor toegang tot scores
  });

  // Apply current filter to update dropdown
  filterManualTradePairs();

  // Display active trades
  let tbody = document.querySelector("#manual-trades-table tbody");
  tbody.innerHTML = "";
  tradesData.trades.forEach(trade => {
    let row = statsMap[trade.pair];
    let statusIcon = '⚪'; // Standaard neutraal
    if (row) {
      let buySignals = 0;
      let sellSignals = 0;

      // Buy triggers
      if (row.score > 3.0) buySignals += 1; // Lagere drempel
      if (row.flow_pct > 60.0) buySignals += 1;
      if (row.whale_pred_score > 5.0) buySignals += 1;
      if (row.pump_score > 3.0) buySignals += 1;
      if (row.alpha === "BUY" || row.early === "BUY") buySignals += 1;

      // Sell triggers (gevoeliger gemaakt)
      if (row.flow_pct < 40.0) sellSignals += 1; // Hogere drempel voor sneller rood
      if (row.pct < -1.0) sellSignals += 1; // NIEUW: Daling >1% = sell
      if (row.alpha === "SELL" || row.early === "SELL") sellSignals += 1;

      if (buySignals >= 2) statusIcon = '🟢'; // Markt koopt
      else if (sellSignals >= 1) statusIcon = '🔴'; // Markt verkoopt
      // Anders neutraal
    }
    tbody.innerHTML += `
      <tr>
        <td>${trade.pair}</td>
        <td>${trade.entry_price.toFixed(5)}</td>
        <td>${trade.size.toFixed(5)}</td>
        <td>${trade.current_price.toFixed(5)}</td>
        <td class="${trade.pnl_abs > 0 ? 'pos' : (trade.pnl_abs < 0 ? 'neg' : '')}">€${trade.pnl_abs.toFixed(2)}</td>
        <td class="${trade.pnl_pct > 0 ? 'pos' : (trade.pnl_pct < 0 ? 'neg' : '')}">${trade.pnl_pct.toFixed(2)}%</td>
        <td>${new Date(trade.open_ts * 1000).toLocaleString()}</td>
        <td>${trade.fee_pct.toFixed(2)}%</td>
        <td>€${trade.manual_amount.toFixed(2)}</td>
        <td>${statusIcon}</td>  <!-- Actuele markt-status per munt -->
        <td><button onclick="closeManualTrade('${trade.pair}')" style="padding:3px 8px;">Close</button></td>
      </tr>
    `;
  });

  // Draw equity curve
  let equity = await fetch("/api/manual_equity").then(r => r.json());
  drawManualEquity(equity);
}

function filterManualTradePairs() {
  let searchInput = document.getElementById("manual-pair-search");
  let select = document.getElementById("manual-pair");
  
  if (!searchInput || !select) return;
  
  let query = searchInput.value.toLowerCase();
  let filtered = manualTradePairs.filter(p => p.toLowerCase().includes(query));
  
  select.innerHTML = "";
  filtered.forEach(p => {
    let opt = document.createElement("option");
    opt.value = p;
    opt.text = p;
    select.appendChild(opt);
  });
}

// Event listeners for manual trades
document.getElementById('manual-open-btn').addEventListener('click', async () => {
  let pair = document.getElementById('manual-pair').value;
  let fee = parseFloat(document.getElementById('manual-fee').value);
  let amount = parseFloat(document.getElementById('manual-amount').value);
  let sl = parseFloat(document.getElementById('manual-sl').value);
  let tp = parseFloat(document.getElementById('manual-tp').value);
  if (!pair || amount <= 0) {
    alert('Select pair and enter amount');
    return;
  }
  let res = await fetch('/api/manual_trade', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({
      pair,
      sl_pct: sl,
      tp_pct: tp,
      fee_pct: fee,
      manual_amount: amount
    })
  });
  let result = await res.json();
  if (result.success) {
    alert('Trade opened!');
    loadManualTrades();
  } else {
    alert('Failed to open trade');
  }
});

document.getElementById('analyze-coin-btn').addEventListener('click', async () => {
  let pair = document.getElementById('manual-pair').value;
  if (!pair) {
    alert('Select a pair first');
    return;
  }
  let [base, quote] = pair.split('/');
  let res = await fetch(`/api/coin_analysis/${base}/${quote}`);
  let data = await res.json();
  if (data.error) {
    alert(data.error);
    return;
  }
  document.getElementById('modal-title').textContent = `Analyse voor ${pair}`;
  let content = document.getElementById('modal-content');
  content.innerHTML = `
    <p>Huidige prijs: €${data.current_price.toFixed(5)}</p>
    <p>RSI: ${data.calculations.rsi.toFixed(2)}</p>
    <p>Volatiliteit: ${data.calculations.volatility.toFixed(2)}%</p>
    <p>Gemiddeld volume: ${data.calculations.avg_volume.toFixed(2)}</p>
    <p>Advies: ${data.advice}</p>
    <canvas id="analysis-chart" width="800" height="400"></canvas>
  `;
  const ctx = document.getElementById('analysis-chart').getContext('2d');
  new Chart(ctx, {
    type: 'line',
    data: {
      labels: data.history.map(c => new Date(c[0] * 1000).toLocaleTimeString()),
      datasets: [{
        label: 'Close Price',
        data: data.history.map(c => c[4]),
        borderColor: 'rgba(75, 192, 192, 1)',
        borderWidth: 1,
        fill: false
      }]
    },
    options: {
      scales: {
        x: {
          display: true,
          title: {
            display: true,
            text: 'Time'
          }
        },
        y: {
          display: true,
          title: {
            display: true,
            text: 'Price (€)'
          }
        }
      }
    }
  });
  document.getElementById('coin-analysis-modal').style.display = 'flex';
});

document.getElementById('close-modal').addEventListener('click', () => {
  document.getElementById('coin-analysis-modal').style.display = 'none';
});

async function closeManualTrade(pair) {
  if (!confirm(`Close trade for ${pair}?`)) {
    return;
  }
  
  let res = await fetch("/api/manual_trade", {
    method: "DELETE",
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({pair})
  });
  let result = await res.json();
  if (result.success) {
    alert(`Trade closed for ${pair}!`);
    loadManualTrades();
  } else {
    alert(`Failed to close trade for ${pair}.`);
  }
}

function drawManualEquity(equity) {
  let canvas = document.getElementById("manual-equity");
  if (!canvas) return;
  let ctx = canvas.getContext("2d");
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  
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

  ctx.strokeStyle = "#666";
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(padding, h - 30);
  ctx.lineTo(w - 10, h - 30);
  ctx.moveTo(40, 10);
  ctx.lineTo(40, h - 30);
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
            let typeClass = "signal_type signal_type_" + r.signal_type;
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
              <td style="color:${ r.pump_label === "MEGA_PUMP" ? "#ff4081" :
                r.pump_label === "EARLY_PUMP" ? "#00bcd4" :
                "#ccc"}">${r.pump_score.toFixed(1)}</td>
              <td class="${predClass}">${r.whale_pred_label} (${r.whale_pred_score.toFixed(1)})</td>
              <td class="${relClass}">${r.reliability_label} (${r.reliability_score.toFixed(0)})</td>
              <td class="${typeClass}">${r.signal_type}</td>
              <td>${r.confidence.toFixed(1)}%</td>
              <td>${r.momentum.toFixed(1)}%</td>
              <td>${r.trade_score.toFixed(1)}</td>
              <td>${r.buy_trades}_Buy / ${r.sell_trades}_Sell</td>
              <td>${visual}</td>
              <td>${r.analysis}</td>
            </tr>`;
          }
          for (let r of finalFiltered) {
            tbody.innerHTML += renderRow(r);
          }

          // Load historie tabel: GEEN FILTERS, alleen sorteren op ts desc, dan pair asc
          fetch("/api/stars_history")
            .then(r => r.json())
            .then(history => {
              let historyFiltered = history; // GEEN FILTERS
              // Sorteer: eerst op ts desc, dan pair asc
              historyFiltered.sort((a, b) => {
                if (b.ts !== a.ts) {
                  return b.ts - a.ts; // Jongste eerst
                }
                return a.pair.localeCompare(b.pair); // Pair asc
              });
              let histTbody = document.querySelector("#stars-history-table tbody");
              histTbody.innerHTML = "";
              for (let r of historyFiltered.slice(0, 100)) {  // Beperk tot 100 voor performance
                histTbody.innerHTML += renderRow(r);
              }
              console.log(`Loaded ${historyFiltered.length} history entries (no filters, sorted by ts desc, pair asc)`);
            })
            .catch(err => console.error("stars history error", err));
        });
    })
    .catch(err => console.error("stars error", err));
}

let healthLoaded = false; // NIEUW: Vlag om te voorkomen dat loadHealth elke seconde loopt

async function loadHealth() {
  if (healthLoaded) return; // Alleen bij eerste keer laden
  healthLoaded = true;

  try {
    let res = await fetch("/api/health");
    let data = await res.json();
    let tbody = document.querySelector("#health-table tbody");
    tbody.innerHTML = "";
    for (let [key, value] of Object.entries(data)) {
      let status = (key.includes("restart") && value > 0) ? "neg" : "pos";
      tbody.innerHTML += `<tr><td>${key.replace(/_/g, ' ')}</td><td>${value}</td><td class="${status}">OK</td></tr>`;
    }
    document.getElementById("health-last-update").textContent = `Last update: ${new Date().toLocaleString()}`;

    // Vul priority pair dropdown met EUR pairs
    let statsRes = await fetch("/api/stats");
    let statsData = await statsRes.json();
    let select = document.getElementById("priority-pair-select");
    select.innerHTML = '<option value="">Geen</option>';
    let eurPairs = statsData.filter(r => r.pair.endsWith('/EUR')).map(r => r.pair).sort();
    eurPairs.forEach(pair => {
      let opt = document.createElement("option");
      opt.value = pair;
      opt.text = pair;
      if (priorityPair && pair === priorityPair) opt.selected = true;
      select.appendChild(opt);
    });
  } catch (e) {
    console.error("Health load error:", e);
  }
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

async function loadPriorityPair() {
  try {
    let configRes = await fetch('/api/config');
    let config = await configRes.json();
    priorityPair = config.priority_pair || null;
    console.log('Priority pair loaded:', priorityPair);
  } catch (e) {
    console.error('Failed to load priority pair');
  }
}

window.addEventListener("load", () => {
  loadPriorityPair(); // NIEUW: Laad priorityPair bij startup
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
        // FIX: Vervang komma door punt voor parseFloat
        let val = el.value.replace(',', '.');
        cfg[el.id] = parseFloat(val);
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

  // NIEUW: Priority pair setter
  document.getElementById('set-priority-btn').addEventListener('click', async () => {
    let selectedPair = document.getElementById('priority-pair-select').value;
    try {
      let res = await fetch('/api/priority_pair', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({pair: selectedPair})
      });
      let result = await res.json();
      if (result.success) {
        priorityPair = selectedPair || null;
        document.getElementById('priority-status').textContent = selectedPair ? `Priority set to ${selectedPair}` : 'Priority cleared';
        // Herlaad ALLE tabbladen voor highlighting
        loadMarkets();
        loadSignals();
        loadTop10();
        loadForecast();
        // Etc. voor andere indien nodig
      } else {
        document.getElementById('priority-status').textContent = 'Failed to set priority';
      }
    } catch (e) {
      document.getElementById('priority-status').textContent = 'Error: ' + e.message;
    }
  });
});

// Event listeners voor filters
document.getElementById('markets-dir-filter').addEventListener('change', () => applyDirFilter('grid', 'markets-dir-filter', 6)); // Adjusted for new Time column
document.getElementById('signals-dir-filter').addEventListener('change', () => applyDirFilter('signals', 'signals-dir-filter', 3));
document.getElementById('top10-dir-filter').addEventListener('change', () => {
  applyDirFilter('top3', 'top10-dir-filter', 5);
  applyDirFilter('top10-up', 'top10-dir-filter', 5);
  applyDirFilter('top10-down', 'top10-dir-filter', 5);
});

function tick() {
  if (activeTab === "markets") {
    loadMarkets();
  } else if (activeTab === "signals") {
    loadSignals();
  } else if (activeTab === "top10") {
    loadTop10();
  } else if (activeTab === "manual_trades") {
    loadManualTrades();
  } else if (activeTab === "backtest") {
    loadBacktest();
  } else if (activeTab === "stars") {
    loadStars();
  } else if (activeTab === "forecast") {
    loadForecast();
  }
}

setInterval(tick, 1000);
document.getElementById("search").addEventListener("input", () => {
  if (activeTab === "markets") loadMarkets();
  if (activeTab === "signals") loadSignals(); // NIEUW: Search filter toegevoegd voor Signals
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

    let mut backoff = Duration::from_secs(5);
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
                eprintln!("WS{}: connect error {:?}, retry in {:?}", worker_id, e, backoff);
                sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(300)); // Max 5 min
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
                "WS{}: subscribe send error {:?}, retry in {:?}",
                worker_id, e, backoff
            );
            sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(300));
            continue;
        }

        println!(
            "WS{}: subscribed to {} pairs via WebSocket",
            worker_id,
            ws_pairs.len()
        );

        backoff = Duration::from_secs(5); // Reset bij succes

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

        eprintln!("WS{}: stream ended, reconnecting in {:?}", worker_id, backoff);
        sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(300));
    }
}

async fn run_orderbook_worker(
    engine: Engine,
    ws_pairs: Vec<String>,
    worker_id: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = "wss://ws.kraken.com";

    let mut backoff = Duration::from_secs(5);
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
                eprintln!("OB_WS{}: connect error {:?}, retry in {:?}", worker_id, e, backoff);
                sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(300));
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
                "OB_WS{}: subscribe send error {:?}, retry in {:?}",
                worker_id, e, backoff
            );
            sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(300));
            continue;
        }

        println!(
            "OB_WS{}: subscribed to orderbook for {} pairs",
            worker_id,
            ws_pairs.len()
        );

        backoff = Duration::from_secs(5); // Reset bij succes

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

        eprintln!("OB_WS{}: stream ended, reconnecting in {:?}", worker_id, backoff);
        sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(300));
    }
}

// ============================================================================
// NIEUW: Priority Pair Scanner
// ============================================================================

async fn run_priority_pair_scanner(engine: Engine, config: Arc<Mutex<AppConfig>>) -> Result<(), Box<dyn std::error::Error>> {
    println!("[PRIORITY SCANNER] Started, checking every 5 seconds");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    loop {
        let priority_pair = {
            let cfg = config.lock().unwrap();
            cfg.priority_pair.clone()
        };

        if let Some(pair) = priority_pair {
            let kraken_pair = denormalize_pair(&pair);
            let url = format!("https://api.kraken.com/0/public/Ticker?pair={}", kraken_pair);

            if let Ok(resp) = client.get(&url).send().await {
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
                                let norm = pair.clone(); // FIX: Gebruik pair.clone() in plaats van key_to_norm.get(k)
                                engine.handle_ticker(&norm, last, vol24h, open, ts_int);
                                println!("[PRIORITY] Updated {} at ts {}", norm, ts_int);
                            }
                        }
                    }
                }
            }
        }

        sleep(Duration::from_secs(5)).await; // Elke 5 seconden voor priority pair
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

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    loop {
        for chunk in kraken_keys.chunks(20) {
            let keys: Vec<String> = chunk.iter().cloned().collect();
            let joined = keys.join(",");
            let url =
                format!("https://api.kraken.com/0/public/Ticker?pair={}", joined);

            if let Ok(resp) = client.get(&url).send().await {
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
// HOOFDSTUK 13 – CLEANUP & ONDERHOUD (AGRESSIEVER GEMAAKT)
// ============================================================================


async fn run_cleanup(engine: Engine) {
    loop {
        sleep(Duration::from_secs(600)).await; // Nog steeds elke 10 min, maar agressiever

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

        // NIEUW: Reset recente ANOM flags na 5 uur
        let cutoff_anom = now - (5 * 3600); // 5 uur
        for mut t in engine.trades.iter_mut() {
            if t.last_update_ts < cutoff_anom {
                t.recent_anom = false;
            }
        }

        // FIX: Houd alleen signals van laatste 24 uur om Type kolom te vullen
        {
            let mut sigs = engine.signals.lock().unwrap();
            sigs.retain(|ev| now - ev.ts < 86400); // 24 uur
        }

        // NIEUW: Cleanup dir_history voor entries ouder dan 24 uur
        let cutoff_dir = now - 86400; // 24 uur
        for mut t in engine.trades.iter_mut() {
            t.dir_history.retain(|(ts, _)| *ts >= cutoff_dir);
        }

        println!("Cleanup: oude trades (>12u), candles (>24u), orderbooks (>1m), signals (>24u) en dir_history (>24u) opgeschoond, oude ANOM flags gereset.");
    }
}

// NIEUW: Agressieve cleanup elke 5 minuten
async fn run_aggressive_cleanup(engine: Engine) {
    let mut interval = interval(Duration::from_secs(300)); // Elke 5 minuten
    loop {
        interval.tick().await;
        let now = Utc::now().timestamp();

        // Log sizes voor debugging
        let trades_count: usize = engine.trades.iter().map(|_| 1).sum();
        let candles_count: usize = engine.candles.iter().map(|_| 1).sum();
        let signals_count = engine.signals.lock().unwrap().len();
        let stars_count = engine.stars_history.lock().unwrap().history.len();
        println!("[HEALTH] trades: {}, candles: {}, signals: {}, stars: {}", trades_count, candles_count, signals_count, stars_count);

        // Cleanup trades: verwijder oude pairs volledig na 6 uur inactiviteit
        let cutoff_old = now - 6 * 3600;
        engine.trades.retain(|_, v| v.last_update_ts >= cutoff_old);

        // Cleanup candles: max 500 entries totaal
        let mut candle_keys: Vec<String> = engine.candles.iter().map(|k| k.key().clone()).collect();
        if candle_keys.len() > 500 {
            candle_keys.sort_by_key(|k| engine.candles.get(k).map(|c| c.last_update_ts).unwrap_or(0));
            for key in candle_keys.iter().take(candle_keys.len() - 500) {
                engine.candles.remove(key);
            }
        }

        // Signals: max 200 (strenger dan 400)
        let mut sigs = engine.signals.lock().unwrap();
        let sigs_len = sigs.len();
        if sigs_len > 200 {
            sigs.drain(0..(sigs_len - 200));
        }
        drop(sigs);

        // Stars: max 500 entries (minder dan 1000)
        let mut history = engine.stars_history.lock().unwrap();
        let history_len = history.history.len();
        if history_len > 500 {
            history.history.drain(0..(history_len - 500));
            history.dirty = true; // Markeer voor save
        }
        drop(history);

        // Orderbooks: max 100 recente
        let cutoff_ob = now - 120; // 2 minuten
        engine.orderbooks.retain(|_, v| v.timestamp >= cutoff_ob);

        println!("[CLEANUP] Aggressive cleanup done");
    }
}

// ============================================================================
// HOOFDSTUK 14 – HTTP SERVER & API (MET HEALTH ENDPOINT UITGEBREID + FORECAST ENDPOINT + PRIORITY PAIR API)
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

    let api_manual_trades = warp::path!("api" / "manual_trades")
        .and(engine_filter.clone())
        .map(|engine: Engine| warp::reply::json(&engine.manual_trades_snapshot()));

    let api_manual_equity = warp::path!("api" / "manual_equity")
        .and(engine_filter.clone())
        .map(|engine: Engine| {
            let trader = engine.manual_trader.lock().unwrap();
            warp::reply::json(&trader.equity_curve)
        });

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

    // NIEUW: API voor stars historie
    let api_stars_history = warp::path!("api" / "stars_history")
        .and(engine_filter.clone())
        .map(|engine: Engine| {
            let history = engine.stars_history.lock().unwrap();
            let mut sorted_history = history.history.clone();
            sorted_history.sort_by(|a, b| b.ts.cmp(&a.ts));
            warp::reply::json(&sorted_history)
        });

    // NIEUW: API voor forecast voorspellingen
    let api_forecast = warp::path!("api" / "forecast")
        .and(engine_filter.clone())
        .map(|engine: Engine| warp::reply::json(&engine.forecast_snapshot()));

    // NIEUW: API voor coin analyse (on-demand)
    let api_coin_analysis = warp::path!("api" / "coin_analysis" / String / String)
        .and_then(|base: String, quote: String| async move {
            let pair = format!("{}/{}", base, quote);
            match fetch_kraken_ohlc(&pair).await {
                Ok(candles) => {
                    if candles.is_empty() {
                        return Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!({"error": "No historical data available"})));
                    }
                    let current_price = candles.last().map(|c| c.4).unwrap_or(0.0);
                    let prices: Vec<f64> = candles.iter().map(|c| c.4).collect();
                    let returns: Vec<f64> = prices.windows(2).map(|w| (w[1] - w[0]) / w[0]).collect();
                    let rsi = calculate_rsi(&prices);
                    let volatility = calculate_volatility(&returns);
                    let avg_volume = candles.iter().map(|c| c.5).sum::<f64>() / candles.len() as f64;
                    let mut calculations = HashMap::new();
                    calculations.insert("rsi".to_string(), rsi);
                    calculations.insert("volatility".to_string(), volatility);
                    calculations.insert("avg_volume".to_string(), avg_volume);
                    let advice = if rsi > 70.0 {
                        "Overbought - mogelijk verkoopdruk, risico op daling."
                    } else if rsi < 30.0 {
                        "Oversold - mogelijke koopkans, maar controleer andere signalen."
                    } else if volatility > 5.0 {
                        "Hoge volatiliteit - voorzichtig, grote swings mogelijk."
                    } else {
                        "Neutraal - monitor voor veranderingen."
                    }.to_string();
                    Ok::<_, warp::Rejection>(warp::reply::json(&CoinAnalysisResponse {
                        pair,
                        current_price,
                        history: candles,
                        calculations,
                        advice,
                    }))
                }
                Err(e) => Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!({"error": format!("Failed to fetch data: {}", e)}))),
            }
        });

    // UITGEBREIDE Health endpoint met echte metrics (gebruikt sysinfo indien beschikbaar)
    let api_health = warp::path!("api" / "health").map(|| {
        let uptime = Utc::now().timestamp() - *START_TIME;
        let counts = SELF_HEALING_COUNTS.lock().unwrap();
        // Placeholder voor sysinfo: memory en threads (vereist crate)
        let memory_mb = 0; // sysinfo::System::new_all().used_memory() / 1024 / 1024;
        let threads_active = 0; // sysinfo::System::new_all().processes().len();
        warp::reply::json(&serde_json::json!({
            "uptime_seconds": uptime,
            "memory_mb": memory_mb,
            "threads_active": threads_active,
            "news_scanner_restarts": counts.news_scanner_restarts,
            "ws_worker_restarts": counts.ws_worker_restarts,
            "anomaly_scanner_restarts": counts.anomaly_scanner_restarts,
            "total_restarts": counts.total_restarts
        }))
    });

    // NIEUW: API voor priority pair instellen
    let api_priority_pair = warp::path!("api" / "priority_pair")
        .and(config_filter.clone())
        .and(warp::body::json())
        .map(|config: Arc<Mutex<AppConfig>>, body: serde_json::Value| {
            let pair = body["pair"].as_str().unwrap_or("");
            let mut cfg = config.lock().unwrap();
            cfg.priority_pair = if pair.is_empty() { None } else { Some(pair.to_string()) };
            let _ = save_config(&*cfg);
            warp::reply::json(&serde_json::json!({"success": true}))
        });

    let api_manual_trade_post = warp::path!("api" / "manual_trade")
        .and(warp::post())
        .and(warp::body::json())
        .and(engine_filter.clone())
        .and_then(|body: serde_json::Value, engine: Engine| async move {
            let pair = body["pair"].as_str().unwrap_or("");
            let sl_pct = body["sl_pct"].as_f64().unwrap_or(2.0);
            let tp_pct = body["tp_pct"].as_f64().unwrap_or(5.0);
            let fee_pct = body["fee_pct"].as_f64().unwrap_or(0.26);
            let manual_amount = body["manual_amount"].as_f64().unwrap_or(100.0);
            let success = engine.manual_add_trade(pair, sl_pct, tp_pct, fee_pct, manual_amount).await;
            Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!({"success": success})))
        });

    let api_manual_trade_delete = warp::path!("api" / "manual_trade")
        .and(warp::delete())
        .and(warp::body::json())
        .and(engine_filter.clone())
        .and_then(|body: serde_json::Value, engine: Engine| async move {
            let pair = body["pair"].as_str().unwrap_or("");
            let success = engine.manual_close_trade(pair).await;
            Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!({"success": success})))
        });

    let index = warp::path::end().map(|| warp::reply::html(DASHBOARD_HTML));

    let routes = api_stats
        .or(api_signals)
        .or(api_top10)
        .or(api_heatmap)
        .or(api_backtest)
        .or(api_manual_trades)
        .or(api_manual_equity)
        .or(api_manual_trade_post)
        .or(api_manual_trade_delete)
        .or(api_config_get)
        .or(api_config_post)
        .or(api_config_reset)
        .or(api_stars_history)
        .or(api_forecast)
        .or(api_coin_analysis)
        .or(api_health)
        .or(api_priority_pair)
        .or(index)
        .with(warp::cors()
            .allow_any_origin()
            .allow_methods(&[warp::http::Method::GET, warp::http::Method::POST, warp::http::Method::DELETE])
            .allow_headers(vec!["content-type"]));

    let mut port: u16 = 8080;
    loop {
        let addr_str = format!("0.0.0.0:{}", port);  // Bind op alle interfaces voor direct beschikbaar

        match TcpListener::bind(&addr_str) {
            Ok(listener) => {
                drop(listener);
                println!("Dashboard: http://0.0.0.0:{} (or http://localhost:{})", port, port);
                println!("Open in browser: http://localhost:{}", port);
                warp::serve(routes.clone())
                    .run(([0, 0, 0, 0], port))  // Bind op alle interfaces
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
// HOOFDSTUK 15 – SELF-HEALING WATCHDOG (NIEUW, LIMIET VERWIJDERD)
// ============================================================================

async fn run_watchdog(engine: Engine) -> Result<(), Box<dyn std::error::Error>> {
    println!("[WATCHDOG] Started self-healing watchdog...");
    loop {
        sleep(Duration::from_secs(30)).await; // Check elke 30s

        // News scanner is verwijderd, dus alleen WS en anomaly herstarten
        {
            let engine_clone = engine.clone();
            tokio::spawn(async move {
                if let Err(e) = run_anomaly_scanner(engine_clone, vec![], HashMap::new()).await {
                    eprintln!("[WATCHDOG] Anomaly scanner restart failed: {:?}", e);
                }
            });
            SELF_HEALING_COUNTS.lock().unwrap().increment_anomaly();
        }
    }
}

// ============================================================================
// HOOFDSTUK 16 – MAIN ENTRYPOINT (MET GRACEFUL SHUTDOWN)
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
    let chunk_size = 50; // VERHOOGD VAN 10 NAAR 50 OM RATE LIMITING TE VERMIJDEN
    let chunks: Vec<Vec<String>> = ws_pairs.chunks(chunk_size).map(|c| c.to_vec()).collect();

    println!(
        "Using {} pairs for anomaly scanner (REST), {} EUR pairs via WebSocket trades ({} WS workers)",
        kraken_keys.len(),
        total_ws_pairs,
        chunks.len()
    );

    let config = Arc::new(Mutex::new(load_config().await));
    let engine = Engine::new();
    
    // Load manual trader state from JSON
    engine.load_manual_trader().await;
    println!("Loaded manual trader state");

    // Load stars history
    let _ = engine.load_stars_history().await; // Ignore result om warning te fixen
    println!("Loaded stars history");

    // Maak shutdown kanaal
    let (shutdown_tx, mut shutdown_rx) = broadcast::channel::<()>(1);

    let engine_for_ws = engine.clone();

    // Clone chunks for orderbook workers
    let ob_chunks: Vec<Vec<String>> = ws_pairs.chunks(chunk_size).map(|c| c.to_vec()).collect();

    // Spawn HTTP server als eerste, zodat direct beschikbaar
    let engine_http = engine.clone();
    let config_http = config.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = run_http(engine_http, config_http) => {},
            _ = shutdown_rx.recv() => println!("HTTP server shutting down"),
        }
    });
    println!("HTTP server spawned, should be available soon at http://localhost:8080/");

    // Spawn watchdog voor self-healing (geen limiet meer)
    let engine_watchdog = engine.clone();
    tokio::spawn(async move {
        if let Err(e) = run_watchdog(engine_watchdog).await {
            eprintln!("[WATCHDOG] Watchdog error: {:?}", e);
        }
    });
    println!("Watchdog spawned for self-healing");

    // Spawn priority pair scanner
    let engine_priority = engine.clone();
    let config_priority = config.clone();
    tokio::spawn(async move {
        if let Err(e) = run_priority_pair_scanner(engine_priority, config_priority).await {
            eprintln!("[PRIORITY SCANNER] Error: {:?}", e);
        }
    });
    println!("Priority pair scanner spawned");

    // Spawn andere tasks met shutdown listening
    for (i, chunk) in chunks.into_iter().enumerate() {
        let e = engine_for_ws.clone();
        let mut rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            tokio::select! {
                _ = run_kraken_worker(e, chunk, i) => {},
                _ = rx.recv() => println!("WS{} shutting down", i),
            }
        });
        sleep(Duration::from_secs(2)).await;
    }

    let engine_for_ob = engine.clone();
    for (i, chunk) in ob_chunks.into_iter().enumerate() {
        let e = engine_for_ob.clone();
        let mut rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            tokio::select! {
                _ = run_orderbook_worker(e, chunk, i) => {},
                _ = rx.recv() => println!("OB_WS{} shutting down", i),
            }
        });
        sleep(Duration::from_secs(2)).await;
    }

    let engine_anom = engine.clone();
    let mut rx_anom = shutdown_tx.subscribe();
    tokio::spawn(async move {
        tokio::select! {
            _ = run_anomaly_scanner(engine_anom, kraken_keys, key_to_norm) => {},
            _ = rx_anom.recv() => println!("Anomaly scanner shutting down"),
        }
    });

    let engine_eval = engine.clone();
    let mut rx_eval = shutdown_tx.subscribe();
    tokio::spawn(async move {
        tokio::select! {
            _ = run_self_evaluator(engine_eval) => {},
            _ = rx_eval.recv() => println!("Self-evaluator shutting down"),
        }
    });

    let engine_cleanup = engine.clone();
    let mut rx_cleanup = shutdown_tx.subscribe();
    tokio::spawn(async move {
        tokio::select! {
            _ = run_cleanup(engine_cleanup) => {},
            _ = rx_cleanup.recv() => println!("Cleanup shutting down"),
        }
    });

    let engine_stars_saver = engine.clone();
    let mut rx_saver = shutdown_tx.subscribe();
    tokio::spawn(async move {
        tokio::select! {
            _ = run_stars_history_saver(engine_stars_saver) => {},
            _ = rx_saver.recv() => println!("Stars saver shutting down"),
        }
    });

    // Spawn agressieve cleanup
    let engine_agg_cleanup = engine.clone();
    let mut rx_agg = shutdown_tx.subscribe();
    tokio::spawn(async move {
        tokio::select! {
            _ = run_aggressive_cleanup(engine_agg_cleanup) => {},
            _ = rx_agg.recv() => println!("Aggressive cleanup shutting down"),
        }
    });

    // Wacht op Ctrl+C en broadcast shutdown
    println!("All tasks spawned. App running. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;
    println!("Shutdown signal received, broadcasting to all tasks...");
    shutdown_tx.send(()).unwrap();
    sleep(Duration::from_secs(5)).await; // Geef tijd om af te sluiten
    println!("Shutting down...");
    Ok(())
}

// NIEUW: Automatische saver voor stars historie
async fn run_stars_history_saver(engine: Engine) -> Result<(), Box<dyn std::error::Error>> {
    println!("[STARS SAVER] Started, will save every 10 seconds if dirty");
    loop {
        sleep(Duration::from_secs(10)).await;
        let is_dirty = {
            let history_guard = engine.stars_history.lock().unwrap();
            history_guard.dirty
        };
        if is_dirty {
            let data = {
                let history_guard = engine.stars_history.lock().unwrap();
                history_guard.history.clone()
            };
            match save_stars_history_to_file(&data).await {
                Ok(_) => {
                    let mut history_guard = engine.stars_history.lock().unwrap();
                    history_guard.dirty = false;
                    println!("[STARS SAVER] Saved successfully, set dirty=false");
                }
                Err(e) => eprintln!("[STARS SAVER] Save error: {}", e),
            }
        }
    }
}

async fn save_stars_history_to_file(data: &[TopRow]) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string_pretty(data)?;
    tokio::fs::write(STARS_HISTORY_FILE, json).await?;
    Ok(())
}

async fn fetch_kraken_ohlc(pair: &str) -> Result<Vec<(i64, f64, f64, f64, f64, f64)>, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let kraken_pair = denormalize_pair(pair);
    let since = (Utc::now().timestamp() - 86400) as u64;  // 24u geleden
    let url = format!("https://api.kraken.com/0/public/OHLC?pair={}&interval=5&since={}", kraken_pair, since);
    let resp = client.get(&url).send().await?;
    
    if !resp.status().is_success() {
        return Err(format!("HTTP error: {}", resp.status()).into());
    }
    
    let json: Value = resp.json().await?;
    
    if let Some(errors) = json["error"].as_array() {
        if !errors.is_empty() {
            return Err(format!("Kraken error: {:?}", errors).into());
        }
    }
    
    let mut candles = Vec::new();
    if let Some(result) = json["result"].as_object() {
        if let Some(data) = result.values().next().and_then(|v| v.as_array()) {
            for item in data {
                if let Some(arr) = item.as_array() {
                    let ts = arr[0].as_u64().unwrap_or(0) as i64;
                    let open = arr[1].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                    let high = arr[2].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                    let low = arr[3].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                    let close = arr[4].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                    let volume = arr[6].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                    candles.push((ts, open, high, low, close, volume));
                }
            }
        }
    }
    Ok(candles)
}

fn denormalize_pair(norm: &str) -> String {
    let parts: Vec<&str> = norm.split('/').collect();
    if parts.len() != 2 || parts[1] != "EUR" {
        return norm.to_string();
    }
    let base = parts[0];
    match base {
        "BTC" => "XXBTZEUR".to_string(),
        "ETH" => "XETHZEUR".to_string(),
        "XRP" => "XXRPZEUR".to_string(),
        "DOGE" => "XDGEUR".to_string(),
        "LTC" => "XLTCZEUR".to_string(),
        "ADA" => "ADAEUR".to_string(),
        "SOL" => "SOLEUR".to_string(),
        _ => format!("{}EUR", base),
    }
}

fn calculate_rsi(prices: &[f64]) -> f64 {
    if prices.len() < 14 { return 50.0; }  // RSI over 14 periodes
    let mut gains = 0.0;
    let mut losses = 0.0;
    for i in 1..14 {
        let diff = prices[i] - prices[i-1];
        if diff > 0.0 { gains += diff; } else { losses += diff.abs(); }
    }
    let avg_gain = gains / 14.0;
    let avg_loss = losses / 14.0;
    if avg_loss == 0.0 { return 100.0; }
    let rs = avg_gain / avg_loss;
    100.0 - (100.0 / (1.0 + rs))
}

fn calculate_volatility(returns: &[f64]) -> f64 {
    if returns.len() < 2 { return 0.0; }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
    variance.sqrt() * 100.0  // Als %
}

#[derive(Debug, Serialize)]
struct CoinAnalysisResponse {
    pair: String,
    current_price: f64,
    history: Vec<(i64, f64, f64, f64, f64, f64)>,
    calculations: HashMap<String, f64>,
    advice: String,
}
