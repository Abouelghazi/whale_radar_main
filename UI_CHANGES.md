# UI Changes for Manual Trading

## Before and After Comparison

### BEFORE - Manual Trading Form
```
Open a Trade
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Pair: [Search pair...▼] [BTC/EUR         ▼]

Stop Loss %: [2%  ▼]        Take Profit %: [5%  ▼]

[Open Trade]
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Active Trades
┌──────────┬─────────────┬──────┬───────────────┬─────────┬─────────┬───────────┬────────┐
│   Pair   │ Entry Price │ Size │ Current Price │ PnL Abs │ PnL %   │ Open Time │ Action │
└──────────┴─────────────┴──────┴───────────────┴─────────┴─────────┴───────────┴────────┘
```

### AFTER - Manual Trading Form (Enhanced)
```
Open a Trade
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Pair: [Search pair...▼] [BTC/EUR         ▼]

Notional Amount (€): [100.0  ]        Fee %: [0.1   ]     ← NEW FIELDS

Stop Loss %: [2%  ▼]        Take Profit %: [5%  ▼]

[Open Trade]
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Active Trades
┌──────────┬─────────────┬──────┬───────────────┬─────────────┬─────────┬───────────┬────────┐
│   Pair   │ Entry Price │ Size │ Current Price │ PnL Abs (€) │ PnL %   │ Open Time │ Action │
│          │             │      │               │ (After Fee) │         │           │        │
└──────────┴─────────────┴──────┴───────────────┴─────────────┴─────────┴───────────┴────────┘
                                                  ↑ Now shows fee-adjusted PnL
```

## New UI Elements

### 1. Notional Amount Input
```html
<label style="margin-right:10px;">Notional Amount (€):</label>
<input type="number" id="manual-notional" value="100.0" step="10" min="10" style="width:100px;" />
```
- **Default Value**: 100.0
- **Step**: 10
- **Minimum**: 10
- **Purpose**: Allows users to specify how much money to invest per trade

### 2. Fee Percentage Input
```html
<label style="margin-left:20px; margin-right:10px;">Fee %:</label>
<input type="number" id="manual-fee" value="0.1" step="0.05" min="0" max="5" style="width:80px;" />
```
- **Default Value**: 0.1
- **Step**: 0.05
- **Minimum**: 0
- **Maximum**: 5
- **Purpose**: Specifies the trading fee percentage to deduct from PnL

## User Experience Improvements

### Opening a Trade - Step by Step
1. ✅ Select a trading pair from dropdown (searchable)
2. ✅ **NEW**: Enter notional amount (e.g., €500 instead of hardcoded €100)
3. ✅ **NEW**: Set fee percentage (e.g., 0.1% for realistic trading costs)
4. ✅ Configure stop loss percentage
5. ✅ Configure take profit percentage
6. ✅ Click "Open Trade"

### Active Trades Display
- PnL now shows **fee-adjusted values**
- Example: If raw PnL is €10 and fee is €0.10, displayed PnL is €9.90
- Color coding:
  - Green (positive PnL after fees)
  - Red (negative PnL after fees)

### Automatic Trade Closure
- **NEW**: Trades automatically close when:
  - ✅ Price drops to or below stop loss → Closed automatically
  - ✅ Price rises to or above take profit → Closed automatically
- **Benefit**: No need to manually monitor and close trades
- **Fee Application**: Fees are automatically deducted on closure

## Examples

### Example 1: Basic Trade
```
Pair: BTC/EUR
Notional: €100
Fee: 0.1%
Stop Loss: 2%
Take Profit: 5%

Entry Price: €50,000
Size: 0.002 BTC (€100 / €50,000)

Scenario: Price rises to €52,500 (Take Profit)
Raw PnL: (€52,500 - €50,000) × 0.002 = €5.00
Fee: 0.1% × €100 = €0.10
PnL After Fee: €5.00 - €0.10 = €4.90 ✅ (Automatically closed)
```

### Example 2: Higher Investment
```
Pair: ETH/EUR
Notional: €500
Fee: 0.15%
Stop Loss: 3%
Take Profit: 10%

Entry Price: €2,000
Size: 0.25 ETH (€500 / €2,000)

Scenario: Price drops to €1,940 (Stop Loss)
Raw PnL: (€1,940 - €2,000) × 0.25 = -€15.00
Fee: 0.15% × €500 = €0.75
PnL After Fee: -€15.00 - €0.75 = -€15.75 ❌ (Automatically closed)
```

## Validation Messages

### Client-Side (JavaScript)
- "Please select a pair!" - No pair selected
- "Please enter a valid notional amount!" - Notional ≤ 0 or NaN

### Server-Side (Rust)
- "Failed to open {pair} - position already exists" - Duplicate trade
- "Failed to open {pair} - invalid notional amount: {value}" - Invalid notional
- "Failed to open trade for {pair}. Trade may already exist or price not available." - Generic failure

## Browser Compatibility
- Input type="number" with step/min/max attributes
- Modern browsers (Chrome, Firefox, Safari, Edge)
- Mobile-friendly number input with appropriate keyboards

## Accessibility
- Clear labels for all input fields
- Logical tab order
- Descriptive error messages
- Visual feedback on success/failure
