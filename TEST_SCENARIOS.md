# Test Scenarios for Manual Trading Enhancements

## Overview

These test scenarios help verify that all three features work correctly.

## Test Scenario 1: Basic Trade with Fee

### Objective
Verify that trades can be opened with custom notional amounts and fees are correctly applied.

### Steps
1. Navigate to "Manual Trades" tab
2. Search for "BTC/EUR" in the pair dropdown
3. Enter notional amount: `100`
4. Enter fee: `0.1`
5. Select stop loss: `2%`
6. Select take profit: `5%`
7. Click "Open Trade"

### Expected Results
- ✅ Trade opens successfully
- ✅ Alert shows: "Trade opened for BTC/EUR!"
- ✅ Trade appears in Active Trades table
- ✅ Size = 100 / current_price
- ✅ PnL updates in real-time
- ✅ Console log shows: `[MANUAL TRADE] OPEN BTC/EUR at X.XXXXX size X.XXXXX SL=X.XXXXX TP=X.XXXXX notional=100.00 fee=0.10%`

### Verification
```
Entry Price: €50,000
Notional: €100
Size: 0.002 BTC
Fee: 0.1%

If current price = €50,500:
Raw PnL = (50,500 - 50,000) × 0.002 = €1.00
Fee = 0.1% × €100 = €0.10
Displayed PnL = €1.00 - €0.10 = €0.90 ✅
```

## Test Scenario 2: Automatic Take Profit

### Objective
Verify that trades automatically close when take profit is reached.

### Steps
1. Open a trade with:
   - Pair: `ETH/EUR`
   - Notional: `200`
   - Fee: `0.1`
   - Stop Loss: `2%`
   - Take Profit: `5%`
2. Note the entry price (e.g., €2,000)
3. Calculate TP price: entry × 1.05 (e.g., €2,100)
4. Wait for price to reach or exceed TP

### Expected Results
- ✅ Trade closes automatically when price ≥ TP
- ✅ Console log shows: `[MANUAL TRADE] CLOSED ETH/EUR at X.XXXXX PnL=X.XX Fee=X.XX PnL after fee=X.XX Reason: Take Profit`
- ✅ Trade removed from Active Trades table
- ✅ Balance updated with fee-adjusted PnL
- ✅ Equity curve updated

### Verification
```
Entry Price: €2,000
Exit Price: €2,100 (Take Profit)
Notional: €200
Size: 0.1 ETH
Fee: 0.1%

Raw PnL = (2,100 - 2,000) × 0.1 = €10.00
Fee = 0.1% × €200 = €0.20
Final PnL = €10.00 - €0.20 = €9.80 ✅

Balance should increase by €9.80
```

## Test Scenario 3: Automatic Stop Loss

### Objective
Verify that trades automatically close when stop loss is hit.

### Steps
1. Open a trade with:
   - Pair: `ADA/EUR`
   - Notional: `150`
   - Fee: `0.15`
   - Stop Loss: `3%`
   - Take Profit: `10%`
2. Note the entry price (e.g., €0.50)
3. Calculate SL price: entry × 0.97 (e.g., €0.485)
4. Wait for price to drop to or below SL

### Expected Results
- ✅ Trade closes automatically when price ≤ SL
- ✅ Console log shows: `[MANUAL TRADE] CLOSED ADA/EUR at X.XXXXX PnL=-X.XX Fee=X.XX PnL after fee=-X.XX Reason: Stop Loss`
- ✅ Trade removed from Active Trades table
- ✅ Balance decreased by loss + fee
- ✅ Equity curve updated

### Verification
```
Entry Price: €0.50
Exit Price: €0.485 (Stop Loss)
Notional: €150
Size: 300 ADA
Fee: 0.15%

Raw PnL = (0.485 - 0.50) × 300 = -€4.50
Fee = 0.15% × €150 = €0.225
Final PnL = -€4.50 - €0.225 = -€4.725 ✅

Balance should decrease by €4.725
```

## Test Scenario 4: Multiple Concurrent Trades

### Objective
Verify that multiple trades can be managed simultaneously with different parameters.

### Steps
1. Open Trade 1:
   - Pair: `BTC/EUR`
   - Notional: `100`
   - Fee: `0.1`
2. Open Trade 2:
   - Pair: `ETH/EUR`
   - Notional: `200`
   - Fee: `0.15`
3. Open Trade 3:
   - Pair: `ADA/EUR`
   - Notional: `50`
   - Fee: `0.05`

### Expected Results
- ✅ All three trades open successfully
- ✅ Each trade shows correct size and fees
- ✅ PnL updates independently for each trade
- ✅ SL/TP monitored for all trades
- ✅ Trades can close independently
- ✅ Balance updates correctly after each closure

## Test Scenario 5: Input Validation

### Objective
Verify that input validation works correctly.

### Test 5.1: Invalid Notional
1. Try to open trade with notional = `0`
2. Expected: Alert "Please enter a valid notional amount!"
3. Try with notional = `-100`
4. Expected: Same alert

### Test 5.2: Missing Pair
1. Leave pair dropdown empty
2. Click "Open Trade"
3. Expected: Alert "Please select a pair!"

### Test 5.3: Duplicate Position
1. Open trade for `BTC/EUR`
2. Try to open another trade for `BTC/EUR`
3. Expected: Alert "Failed to open trade for BTC/EUR. Trade may already exist or price not available."
4. Console log: `[MANUAL TRADE] Failed to open BTC/EUR - position already exists`

### Test 5.4: Extreme Fee Values
1. Try fee = `10%` (over max)
2. HTML input should limit to max=5
3. Try fee = `-1%` (negative)
4. HTML input should limit to min=0

## Test Scenario 6: Fee Impact on Different Notional Amounts

### Objective
Verify that fees scale correctly with notional amounts.

### Comparison Table

| Notional | Price  | Size   | Raw PnL | Fee (0.1%) | Final PnL | Fee Impact |
|----------|--------|--------|---------|------------|-----------|------------|
| €50      | €1,000 | 0.05   | €5.00   | €0.05      | €4.95     | 1%         |
| €100     | €1,000 | 0.1    | €10.00  | €0.10      | €9.90     | 1%         |
| €500     | €1,000 | 0.5    | €50.00  | €0.50      | €49.50    | 1%         |
| €1,000   | €1,000 | 1.0    | €100.00 | €1.00      | €99.00    | 1%         |

*Assuming 10% price increase and 0.1% fee*

### Verification Steps
1. Open trades with each notional amount
2. Wait for 10% price increase
3. Verify fee is exactly 0.1% of notional
4. Verify fee impact on PnL is consistent

## Test Scenario 7: Edge Cases

### Test 7.1: Very Small Notional
- Notional: `€10` (minimum)
- Verify trade opens and calculates correctly

### Test 7.2: Very Large Notional
- Notional: `€10,000`
- Verify no overflow or precision issues

### Test 7.3: Zero Fee
- Fee: `0%`
- Verify PnL equals raw PnL exactly

### Test 7.4: Maximum Fee
- Fee: `5%`
- Verify large fee deduction is applied correctly

### Test 7.5: Price at Exact SL/TP
- Open trade with SL at €100.00
- Wait for price to reach exactly €100.00
- Verify trade closes (price ≤ SL condition)

## Test Scenario 8: Performance Test

### Objective
Verify that SL/TP checks don't impact performance.

### Steps
1. Open 5 manual trades on different pairs
2. Monitor CPU usage
3. Monitor WebSocket processing speed
4. Close all trades
5. Repeat with 0 trades

### Expected Results
- ✅ No noticeable performance difference
- ✅ SL/TP checks only run for pairs with active trades
- ✅ WebSocket processing maintains normal speed
- ✅ No lag in UI updates

## Test Scenario 9: State Persistence

### Objective
Verify that trades persist across restarts.

### Steps
1. Open a trade with custom notional and fee
2. Note the trade details
3. Stop the application
4. Restart the application
5. Navigate to Manual Trades tab

### Expected Results
- ✅ Trade still appears in Active Trades
- ✅ All fields preserved (notional, fee, SL, TP)
- ✅ PnL continues to update
- ✅ SL/TP monitoring continues

## Test Scenario 10: Manual Close vs Auto Close

### Objective
Compare manual closure with automatic closure.

### Manual Close Steps
1. Open trade
2. Click "Close" button
3. Observe console log

### Auto Close Steps
1. Open trade with tight SL (e.g., 0.5%)
2. Wait for SL to trigger
3. Observe console log

### Expected Differences
- Manual: `Reason: Manual`
- Auto: `Reason: Stop Loss` or `Reason: Take Profit`
- Both should apply fee correctly

## Automated Test Script Template

```javascript
// Paste into browser console for semi-automated testing

async function testManualTrading() {
  console.log("Starting Manual Trading Tests...");
  
  // Test 1: Open trade with custom parameters
  const response = await fetch("/api/manual_trade", {
    method: "POST",
    headers: {"Content-Type": "application/json"},
    body: JSON.stringify({
      pair: "BTC/EUR",
      sl_pct: 2.0,
      tp_pct: 5.0,
      notional: 100.0,
      fee_pct: 0.1
    })
  });
  
  const result = await response.json();
  console.log("Trade opened:", result.success);
  
  // Wait and check trades
  await new Promise(r => setTimeout(r, 1000));
  const trades = await fetch("/api/manual_trades").then(r => r.json());
  console.log("Active trades:", trades.trades.length);
  console.log("Balance:", trades.balance);
  
  // Test close
  if (trades.trades.length > 0) {
    const pair = trades.trades[0].pair;
    const closeResp = await fetch("/api/manual_trade", {
      method: "DELETE",
      headers: {"Content-Type": "application/json"},
      body: JSON.stringify({pair})
    });
    const closeResult = await closeResp.json();
    console.log("Trade closed:", closeResult.success);
  }
  
  console.log("Tests complete!");
}

// Run tests
testManualTrading();
```

## Success Criteria

All tests pass if:
- ✅ Trades open with custom notional amounts
- ✅ Fees are correctly calculated and applied
- ✅ Take profit triggers automatically
- ✅ Stop loss triggers automatically
- ✅ Multiple trades can coexist
- ✅ Input validation works
- ✅ State persists across restarts
- ✅ Performance is not impacted
- ✅ Balance and equity curve update correctly
- ✅ Console logs are descriptive and accurate
