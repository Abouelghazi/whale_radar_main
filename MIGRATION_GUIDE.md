# Migration Guide for Manual Trading Enhancements

## Overview

This guide helps you migrate existing manual trades to the new format that includes fee percentage and notional amount fields.

## Breaking Changes

The `ManualTrade` struct now includes two new required fields:
- `fee_pct`: Trading fee percentage
- `notional`: Original investment amount in euros

## Who Needs to Migrate?

You need to migrate if:
- ✅ You have existing manual trades in `manual_trades.json`
- ✅ Your trades were created before this update
- ❌ You're starting fresh (no migration needed)

## Migration Options

### Option 1: Close All Existing Trades (Recommended)

The simplest approach:

1. **Before updating the code:**
   - Open the WhaleRadar dashboard
   - Manually close all open trades
   - Note down your current balance

2. **Update the code:**
   - Pull the latest changes
   - Restart the application

3. **Continue trading:**
   - Open new trades with the enhanced features

### Option 2: Manual JSON Migration

If you want to preserve existing trades:

#### Step 1: Backup Your Data
```bash
cp manual_trades.json manual_trades.json.backup
cp manual_trades_equity.json manual_trades_equity.json.backup
```

#### Step 2: Update manual_trades.json

**Old Format:**
```json
{
  "initial_balance": 10000.0,
  "balance": 10050.0,
  "trades": {
    "BTC/EUR": {
      "pair": "BTC/EUR",
      "entry_price": 50000.0,
      "size": 0.002,
      "open_ts": 1734614400,
      "stop_loss": 49000.0,
      "take_profit": 52500.0
    }
  },
  "equity_curve": [
    [1734614400, 10000.0],
    [1734614500, 10050.0]
  ]
}
```

**New Format:**
```json
{
  "initial_balance": 10000.0,
  "balance": 10050.0,
  "trades": {
    "BTC/EUR": {
      "pair": "BTC/EUR",
      "entry_price": 50000.0,
      "size": 0.002,
      "open_ts": 1734614400,
      "stop_loss": 49000.0,
      "take_profit": 52500.0,
      "fee_pct": 0.1,          // NEW FIELD
      "notional": 100.0        // NEW FIELD
    }
  },
  "equity_curve": [
    [1734614400, 10000.0],
    [1734614500, 10050.0]
  ]
}
```

#### Step 3: Calculate Missing Fields

For each trade, add:

1. **fee_pct**: Recommended default is `0.1` (0.1%)
2. **notional**: Calculate from `entry_price × size`

Example calculation:
```
entry_price = 50000.0
size = 0.002
notional = 50000.0 × 0.002 = 100.0
```

#### Step 4: Python Migration Script

Save this as `migrate_trades.py`:

```python
#!/usr/bin/env python3
import json

def migrate_trades(input_file, output_file, default_fee_pct=0.1):
    """Migrate manual trades to new format with fee_pct and notional"""
    
    # Load existing trades
    with open(input_file, 'r') as f:
        data = json.load(f)
    
    # Migrate each trade
    for pair, trade in data.get('trades', {}).items():
        # Calculate notional if not present
        if 'notional' not in trade:
            trade['notional'] = trade['entry_price'] * trade['size']
            print(f"Added notional for {pair}: {trade['notional']:.2f}")
        
        # Add default fee_pct if not present
        if 'fee_pct' not in trade:
            trade['fee_pct'] = default_fee_pct
            print(f"Added fee_pct for {pair}: {trade['fee_pct']:.2f}%")
    
    # Save migrated trades
    with open(output_file, 'w') as f:
        json.dump(data, f, indent=2)
    
    print(f"\nMigration complete! Saved to {output_file}")
    print(f"Total trades migrated: {len(data.get('trades', {}))}")

if __name__ == "__main__":
    migrate_trades('manual_trades.json', 'manual_trades_migrated.json')
```

Run the script:
```bash
python3 migrate_trades.py
# Review the output
mv manual_trades_migrated.json manual_trades.json
```

## Verification Steps

After migration:

1. **Check JSON Format:**
   ```bash
   cat manual_trades.json | python3 -m json.tool
   ```
   Should display valid JSON without errors

2. **Verify Fields:**
   - All trades have `fee_pct` field
   - All trades have `notional` field
   - `notional` ≈ `entry_price × size` (allow for rounding)

3. **Start Application:**
   ```bash
   cargo run
   ```
   Check console for any errors

4. **Open Dashboard:**
   - Navigate to Manual Trades tab
   - Verify all trades display correctly
   - Check PnL values are reasonable

## Rollback Procedure

If something goes wrong:

1. **Stop the application**
2. **Restore backups:**
   ```bash
   cp manual_trades.json.backup manual_trades.json
   cp manual_trades_equity.json.backup manual_trades_equity.json
   ```
3. **Checkout previous version:**
   ```bash
   git checkout <previous-commit>
   ```

## Common Issues

### Issue 1: Invalid JSON After Manual Edit
**Symptom:** Application fails to start with JSON parse error

**Solution:**
```bash
# Validate JSON
cat manual_trades.json | python3 -m json.tool
# Fix syntax errors or restore backup
cp manual_trades.json.backup manual_trades.json
```

### Issue 2: Negative Notional Values
**Symptom:** Trades won't open or display incorrectly

**Solution:**
- Recalculate notional: `entry_price × size`
- Ensure notional > 0
- Check for data corruption in JSON

### Issue 3: Missing Trades After Migration
**Symptom:** Some trades don't appear in the UI

**Solution:**
- Verify JSON structure is correct
- Check all required fields are present
- Ensure `trades` object is not empty

## Best Practices

1. **Always backup before migration**
2. **Test with a single trade first**
3. **Verify calculations manually**
4. **Keep backups for at least a week**
5. **Monitor the application after migration**

## Support

If you encounter issues not covered here:

1. Check the application logs
2. Verify JSON structure matches the new format
3. Test with a fresh installation
4. Create an issue on GitHub with:
   - Error messages
   - JSON structure (sanitized)
   - Steps to reproduce

## Timeline

- **Recommended migration window:** Before deploying to production
- **Deadline:** None (but new trades will use new format)
- **Deprecation:** Old format support may be removed in future versions

## FAQ

**Q: Can I use different fee percentages for different trades?**
A: Yes! Each trade can have its own `fee_pct` value.

**Q: What happens if I forget to migrate?**
A: Application may fail to load trades or crash on startup.

**Q: Can I migrate incrementally?**
A: Not recommended. Migrate all trades at once to ensure consistency.

**Q: Will my equity curve be affected?**
A: No, the equity curve remains unchanged during migration.

**Q: Do I need to recalculate my balance?**
A: No, your balance should remain the same unless fees weren't previously accounted for.
