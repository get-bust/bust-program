# Security

## Audit status

**This program has not had an external audit.** It has had internal reviews, property tests, formal proofs of its
math and random testing. None of those replace an audit. [INVARIANTS.md](INVARIANTS.md) lists what is checked and
what is not.

## Reporting a vulnerability

Do not open a public issue. Report it privately through GitHub's
[security advisories](https://github.com/get-bust/bust-program/security/advisories/new) for this repository, with the
steps to reproduce it. A failing test in `programs/bust/tests/attacks.rs` is the best form of report.

## Scope

In scope: the program in `programs/bust`.

Out of scope: the simulator in `sim`, the app and keepers, and anything that needs the upgrade authority, the admin
or the buyback wallet to act against users. Those are trust assumptions, described in the
[README](README.md#trust-model).

## Known limits

- **Open interest can pass a cap.** Room is reserved at the request, so if the vault shrinks in the seconds before
  a fill, the order still fills. The excess is at most what was already reserved.
- **Utilisation can be pushed up on purpose.** `record_utilization` is permissionless, so a temporary large position
  can raise the vault's share of an epoch's profit and lower the stakers'. It costs real margin and fees, and it
  stays under the caps.
- **Opens share one lock.** `request_open` writes the vault, so all opens across all markets run one after another.
  It costs about 22,000 compute units. Under heavy load this can raise priority fees.
- **Pyth is trusted.** A fill uses the message signed for its time, with confidence and signature checks, but a wrong
  price published by Pyth would be used.
- **Keepers are needed for liveness, not for safety.** Anyone can fill, cancel and liquidate, but if nobody does,
  orders wait and bust positions stay open while the vault carries the loss.
- **A day without any keeper gives traders a choice.** An order expires a day after its fill time and then only
  cancels. If no keeper fills anything for that whole day, a trader who can fetch fill messages could fill the
  orders that went their way and let the rest expire. The expiry is a day long so that this needs every keeper to
  be gone for a day, by which time unliquidated positions are the larger problem.
- **A distribution's price is only as fresh as the market's clock.** The caller of `distribute` cannot go below the
  newest publish time the market has seen, but can choose among messages newer than that. Keepers observe every
  second, so that leaves about a second of choice, and the open profit held back can differ by that second's move.
- **Dust.** If everyone unstakes while rewards are streaming, that stretch of rewards stays in the staker bucket.

## Past findings

Found by the internal reviews and the random walk.

| Date | Severity | Finding | Status |
|---|---|---|---|
| 2026-10-30 | Critical | Unclaimed xBUST was left out of the xBUST rate, so early converters could drain backing that belonged to others | Fixed: `early_converter_cannot_take_unclaimed_xbust_backing` |
| 2026-10-30 | High | `withdraw_fees` let the admin take fees around the distribution | Removed |
| 2026-10-30 | High | The fee wallet PDA could have signed for an arbitrary program | Removed: the call it signed is gone, and the fee wallet now signs token transfers only |
| 2026-10-30 | Medium | A public `fund_epoch` could lock an epoch's pool with one unit of $BUST | Prevented: `only_buyback_authority_funds_and_claims_go_to_the_loser` |
| 2026-10-30 | Medium | Capital put in could have counted as profit | Prevented: `usdc_token_fees_enter_the_vault_as_capital` |
| 2026-10-30 | Medium | New xBUST was opened at 1:1 instead of the current rate | Fixed |
| 2026-10-30 | Low | The same account could be passed twice as writable in `sweep_token_fees` | Fixed |
| 2026-11-07 | Medium | The caller of `distribute` could pick the message within the 5 s age limit that showed the least open profit, so profit that traders were sitting on could be distributed | Fixed: `a_distribution_cannot_reach_back_for_a_price_that_hides_open_profit` |
| 2026-11-07 | High | An open order refused by a cap kept a valid fill price, so its owner could free the cap once the price had moved and fill at the old price. Found by the random walk | Fixed: `a_cap_is_never_a_way_to_hold_an_old_fill_price` |

## Before mainnet

- An external audit.
- A verifiable build of the deployed commit, so that the program on chain can be matched to this source.
- A Squads multisig with a timelock as the upgrade authority and the admin.
- Running the tests on the exact binary that will be deployed.
