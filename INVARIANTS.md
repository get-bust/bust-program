# Invariants

The rules this program must keep, and what checks each one. A rule with no check beside it is not a rule here.

- **Kani** proves the rule for every input in `programs/bust/src/proofs.rs`, within the bounds noted there.
- **Proptest** tries thousands of random inputs in `programs/bust/tests/properties.rs`.
- **Walk** is `programs/bust/tests/random_walk.rs`: random use of the whole protocol, with the rule checked after
  every step. It runs on each push, and at 400 seeds of 500 steps every night.
- **Test** names an integration test that runs the real `bust.so` under LiteSVM.

Kani proves the math functions alone. It does not model accounts, token transfers or the runtime. Those are tested,
which shows the rule held in the cases that ran, not in all cases. See [What is not covered](#what-is-not-covered).

## Money

| # | Rule | Checked by |
|---|---|---|
| M1 | Vault tokens = `cash + user_balances + open_margin + fees_accrued + queue_unclaimed + Σ buckets` | Walk, `random_operations_keep_every_ledger_balanced` |
| M2 | No USDC appears or disappears across the vault and every wallet | Walk |
| M3 | Σ (user balance + reserved) = `user_balances` | Walk, `random_operations_keep_every_ledger_balanced` |
| M4 | Σ open margin of positions = `open_margin` | Walk |
| M5 | Everyone can leave: at the end of a walk no position, order or debt is stuck | Walk |
| M6 | Opening and closing at the same price never gains | `opening_and_closing_at_the_same_price_never_gains`, `tiny_moves_inside_the_dead_band_pay_nothing` |
| M7 | A fee is never more than its amount | Kani `a_fee_is_never_more_than_the_amount`, Proptest `fees_never_exceed_the_amount` |
| M8 | A win never pays more than the raw profit, and a bigger move never pays less | Proptest `a_win_never_pays_more_than_the_raw_profit`, `a_bigger_move_never_pays_less` |
| M9 | Settlement math never panics, for any entry, exit, notional and multiplier | Proptest `settlement_math_never_panics` |

## Orders and prices

| # | Rule | Checked by |
|---|---|---|
| O1 | An order fills at exactly one Pyth message: `publish_time ≥ T` and `prev_publish_time < T` | `an_order_fills_at_exactly_one_price` |
| O2 | Waiting does not change the fill price; that message stays valid however late it is used | `a_fill_price_can_be_used_long_after_it_was_published` |
| O3 | An order cancels only with proof that it cannot fill, so cancelling is no way out of a bad price | `only_a_pyth_outage_lets_an_order_be_cancelled`, `a_fill_message_too_uncertain_to_fill_cancels_the_order`, `an_order_within_the_caps_still_cannot_be_cancelled` |
| O4 | A close order whose position is gone can be cancelled | `a_close_order_whose_position_is_gone_can_be_cancelled` |
| O5 | A price must be of the market's feed, owned by Pyth, and verified by enough signatures | `a_fill_cannot_use_another_feeds_price`, `a_fill_cannot_use_a_price_account_pyth_does_not_own`, `partial_verification_needs_enough_signatures` |
| O6 | The market's publish time only moves forward, so a lagging clock gives away no fill price | `fills_and_liquidations_move_the_market_clock_forward_only`, `a_lagging_chain_clock_does_not_give_away_the_fill_price` |
| O7 | An order fills once | `an_order_fills_once_and_a_filled_order_cannot_be_cancelled` |
| O8 | An open fills only if confidence / price × leverage ≤ 25% of the margin | `high_leverage_needs_a_tight_confidence_interval` |

## Open interest

| # | Rule | Checked by |
|---|---|---|
| I1 | OI + pending + a new order stays within both caps at the request | `open_interest_cap_follows_vault`, `all_markets_on_one_side_share_one_cap`, `waiting_orders_hold_their_room_under_the_caps` |
| I2 | An order the caps refuse leaves nothing waiting, so a cap cannot hold an old fill price | `a_cap_is_never_a_way_to_hold_an_old_fill_price` |
| I3 | A fill is never refused by a cap, even if the vault has shrunk since the request | Walk, `a_waiting_order_fills_even_if_the_vault_shrinks` |
| I4 | Market pending = Σ notional of its waiting open orders, and a fill or cancel releases it | Walk |
| I5 | Market OI = Σ notional of its open positions, per side | Walk |
| I6 | The cap is never below its floor | Kani `the_cap_never_falls_below_its_floor`, Proptest `the_oi_cap_is_the_vault_share_or_the_floor` |
| I7 | Past `max_debt` the floors shrink but never close | Proptest `floors_shrink_with_debt_but_never_close` |
| I8 | Utilisation stays within 100% | Kani `utilization_stays_within_a_million`, Proptest `utilization_is_a_share` |

I3 means open interest can pass a cap for as long as that order stays open, by at most what was reserved when the
vault was larger. This is the price of I2.

## Liquidation

| # | Rule | Checked by |
|---|---|---|
| L1 | The bust price sits on the losing side of the entry | Kani `bust_price_never_panics_and_sits_on_the_losing_side`, Proptest `bust_price_sits_on_the_losing_side` |
| L2 | A price clearly past the bust price is bust | Kani `a_price_clearly_bust_is_bust` |
| L3 | Valid params never open a position that is already bust | Proptest `valid_params_never_open_a_bust_position` |
| L4 | A liquidation needs a fresh, certain price from after the fill, and cannot go back in time | `a_liquidation_needs_a_fresh_price`, `a_price_from_before_the_fill_cannot_liquidate`, `a_liquidation_cannot_go_back_in_time`, `an_uncertain_price_does_not_liquidate` |

## Queue

| # | Rule | Checked by |
|---|---|---|
| Q1 | Tickets are paid in order, and never beyond their amount | Kani `a_ticket_is_paid_in_order_and_never_beyond_its_amount`, `tickets_are_paid_in_order` |
| Q2 | `queued_cumulative − paid_cumulative = queue_total` | Walk, `random_operations_keep_every_ledger_balanced` |
| Q3 | Σ user queued = `queue_total + queue_unclaimed` | Walk, `random_operations_keep_every_ledger_balanced` |
| Q4 | A ticket pays only its owner, and nobody can block a close through a ticket address | `a_ticket_pays_only_its_owner`, `lamports_sent_to_the_next_ticket_address_do_not_block_a_close` |

## Distribution, staking and xBUST

| # | Rule | Checked by |
|---|---|---|
| D1 | The split hands out exactly the pool | Kani `the_split_hands_out_exactly_the_pool`, Proptest `the_split_adds_up_to_the_pool` |
| D2 | Nothing is distributed until the queue is empty and past the high-water mark | `distribute_waits_for_empty_queue`, `high_water_mark_blocks_profit_until_recovered` |
| D3 | Profit that open positions are sitting on is not distributed | `profit_open_positions_are_up_is_not_distributed` |
| D3a | The caller of `distribute` cannot give a price older than the newest the market has seen | `a_distribution_cannot_reach_back_for_a_price_that_hides_open_profit` |
| D4 | Capital put in is never distributed as profit | `distribute_splits_fees_and_profit`, `usdc_token_fees_enter_the_vault_as_capital` |
| D5 | Buckets pay only registered wallets, and the staker bucket never leaves through `withdraw_bucket` | `buckets_go_only_to_registered_wallets` |
| D6 | Stake tokens back every stake, and the staker bucket covers every reward earned | Walk |
| D7 | xBUST never loses backing: a convert or a funding cannot lower the $BUST behind each xBUST | Walk, `early_converter_cannot_take_unclaimed_xbust_backing` |
| D8 | Σ epoch loss records = `epoch_loss` | Walk, `random_operations_keep_every_ledger_balanced` |
| D9 | Utilisation is weighted by time: sampling the same moment many times adds no weight | `a_brief_spike_does_not_move_the_vault_share` |
| D10 | `record_utilization` refuses a market list that is incomplete or out of order | `utilization_needs_every_market_in_order`, `record_utilization_rejects_fake_market` |
| D11 | Staking just before a distribution and leaving just after earns next to nothing | `staking_around_a_distribution_earns_next_to_nothing`, `late_staker_earns_only_while_staked` |
| D12 | xBUST claims never pass the epoch's pool | Walk, `xbust_is_shared_by_net_loss` |
| D13 | xBUST is minted only to the loser | `only_buyback_authority_funds_and_claims_go_to_the_loser` |

## Authority

| # | Rule | Checked by |
|---|---|---|
| A1 | Nobody closes, withdraws from or claims on someone else's account | `nobody_closes_someone_elses_position`, `nobody_withdraws_from_someone_elses_account`, `nobody_claims_someone_elses_rewards`, `nobody_stakes_or_locks_with_someone_elses_tokens` |
| A2 | A session key trades but cannot withdraw | `session_key_trades_but_cannot_withdraw`, `session_key_cannot_withdraw` |
| A3 | A deposit address credits only its owner, and only USDC is swept | `deposit_address_credits_its_owner`, `a_deposit_cannot_be_swept_into_another_balance`, `only_usdc_is_swept` |
| A4 | Only the admin configures, and params stay within validated bounds | `only_admin_updates_config`, `config_rejects_invalid_params` |
| A5 | Rent goes back only to whoever paid it | `rent_goes_back_only_to_whoever_paid_it` |
| A6 | Nothing initialises twice | `nothing_initialises_twice`, `initialize_runs_once` |
| A7 | A fee leaves the fee wallet only for the vault or the registered swap account, with the token program that owns it; operations take at most a fifth of it, to their registered account | `usdc_token_fees_enter_the_vault_as_capital`, `non_usdc_token_fees_go_only_to_the_swap_wallet`, `token_2022_fees_go_only_to_the_swap_wallet`, `operations_take_their_share_and_the_rest_is_capital`, `the_operations_share_is_capped_and_the_admin_can_lower_it` |
| A8 | Pause locks no funds: close, liquidate and withdraw work while paused | `pause_locks_no_funds`, `pause_stops_new_positions_only` |

## What is not covered

- **`raw_pnl`, `win_breakdown` and vesting have no Kani proof.** The solver timed out even with bounded inputs. They
  are covered by Proptest and the Walk, so an edge case can still be missed.
- **Kani's bounds.** Several proofs fix a parameter or limit amounts to `2^48` units (about 281 million USDC).
- **The Walk runs one market.** Across markets the caps have their own tests, but not the random walk.
- **The tests run the SBPF v0 build from `target/deploy`.** A binary built for another SBPF version needs them run
  again on it.
- **The upgrade authority is outside every rule here.** It can replace the program. See the trust model in the
  [README](README.md#trust-model).
- **Economics are not invariants.** Whether the vault earns over time is a simulation result, not a guarantee.
