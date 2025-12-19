# Manual Trading Enhancements

This document describes the enhancements made to the manual trading system in WhaleRadar.

## Changes Overview

### 1. Configurable Notional Amount
- **Feature**: Users can now specify the amount (in €) they want to invest per trade
- **Default**: €100.00
- **Implementation**: 
  - Added `notional` field to `ManualTrade` struct
  - Updated UI to include "Notional Amount (€)" input field
  - Modified trade calculation: `size = notional / price`

### 2. Trading Fee Percentage
- **Feature**: Users can specify a fee percentage that is deducted from PnL when trades close
- **Default**: 0.1%
- **Implementation**:
  - Added `fee_pct` field to `ManualTrade` struct
  - Updated UI to include "Fee %" input field
  - Fee is calculated on notional amount: `fee_amount = (fee_pct / 100.0) * notional`
  - PnL after fee: `pnl_after_fee = pnl - fee_amount`

### 3. Automatic Stop-Loss and Take-Profit Execution
- **Feature**: Trades now automatically close when price levels reach SL or TP thresholds
- **Implementation**:
  - Added `check_manual_trade_stops()` method to Engine
  - Called from `handle_trade()` on every price update
  - Checks conditions:
    - Stop Loss: `current_price <= stop_loss`
    - Take Profit: `current_price >= take_profit`
  - Automatic closure includes fee deductions

## Code Changes

### Backend (Rust)

#### ManualTrade Struct
```rust
struct ManualTrade {
    pair: String,
    entry_price: f64,
    size: f64,
    open_ts: i64,
    stop_loss: f64,
    take_profit: f64,
    fee_pct: f64,      // NEW
    notional: f64,     // NEW
}
```

#### Key Functions Modified
1. `ManualTraderState::add_trade()` - Now accepts `notional` and `fee_pct` parameters
2. `ManualTraderState::close_trade()` - Applies fee deduction to PnL
3. `Engine::manual_add_trade()` - Passes new parameters
4. `Engine::check_manual_trade_stops()` - NEW: Checks SL/TP levels automatically
5. `Engine::manual_trades_snapshot()` - Displays fee-adjusted PnL

### Frontend (HTML/JavaScript)

#### UI Changes
- Added input field: `<input type="number" id="manual-notional" value="100.0" .../>`
- Added input field: `<input type="number" id="manual-fee" value="0.1" .../>`

#### JavaScript Changes
- Updated POST request body to include `notional` and `fee_pct`
- Added validation for notional amount (must be > 0)

## API Changes

### POST /api/manual_trade
**Old Request Body:**
```json
{
  "pair": "BTC/EUR",
  "sl_pct": 2.0,
  "tp_pct": 5.0
}
```

**New Request Body:**
```json
{
  "pair": "BTC/EUR",
  "sl_pct": 2.0,
  "tp_pct": 5.0,
  "notional": 100.0,    // NEW
  "fee_pct": 0.1        // NEW
}
```

## Usage Example

### Opening a Trade
1. Navigate to "Manual Trades" tab
2. Select a trading pair
3. Set notional amount (e.g., €500)
4. Set fee percentage (e.g., 0.1%)
5. Set stop loss percentage (e.g., 2%)
6. Set take profit percentage (e.g., 5%)
7. Click "Open Trade"

### Automatic Closure
- Trade will automatically close when:
  - Price drops to or below stop loss level
  - Price rises to or above take profit level
- Fee is automatically deducted from PnL

## Validation

### Notional Amount
- Must be greater than 0
- Validation occurs both in UI (JavaScript) and backend (Rust)

### Fee Percentage
- Can be 0 or positive
- Typical values: 0.05% - 0.5%

## Backward Compatibility

⚠️ **Important**: Existing manual trades saved before this update will need to be migrated.

If you have existing trades in `manual_trades.json`, you'll need to:
1. Back up the file
2. Either manually close existing trades, or
3. Add default values for `fee_pct` and `notional` fields to existing trade entries

Default values for migration:
- `fee_pct`: 0.1
- `notional`: 100.0

## Testing Recommendations

1. **Basic Trade Flow**: Open a trade with custom notional and fee
2. **Fee Calculation**: Verify PnL reflects fee deductions
3. **Stop Loss**: Open a trade and verify it closes when SL is hit
4. **Take Profit**: Open a trade and verify it closes when TP is hit
5. **Edge Cases**: Test with notional = 0 (should fail), very high fees, etc.

## Future Enhancements

Potential improvements for consideration:
- Global default fee percentage in AppConfig
- Trade history with fee breakdown
- Fee statistics and reporting
- Dynamic fee calculation based on market conditions
