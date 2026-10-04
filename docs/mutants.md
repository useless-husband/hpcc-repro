# Mutation check

Each mutant is a deliberate one-line bug applied to a copy of the repository; the full test
suite (`cargo test --release`) must fail. Regenerate with `make mutants`.

| Mutant | File | Bug | Result | Failing tests |
|---|---|---|---|---|
| `hpcc_wrong_hop_qlen` | `src/cc/hpcc.rs` | HPCC reads the queue length of the first hop instead of hop i | killed | cc::hpcc::tests::reacts_to_most_congested_hop, golden_digest, hpcc_reacts_to_the_last_hop_of_a_multi_hop_path |
| `hpcc_wc_every_ack` | `src/cc/hpcc.rs` | HPCC updates the reference window W^c on every ACK (overreaction) | killed | cc::hpcc::tests::same_rtt_acks_do_not_compound, every_cc_converges_to_fair_share, golden_digest, hpcc_drains_incast_in_a_few_rtts_without_overreacting, hpcc_reacts_to_the_last_hop_of_a_multi_hop_path, hpcc_steady_state_queue_is_near_zero, single_long_flow_reaches_line_rate_for_every_cc |
| `hpcc_ignores_eta` | `src/cc/hpcc.rs` | HPCC targets 100% utilisation instead of eta | killed | cc::hpcc::tests::same_rtt_acks_do_not_compound, cc::hpcc::tests::single_congested_hop_sets_window_in_one_step, golden_digest, single_long_flow_reaches_line_rate_for_every_cc |
| `hpcc_tau_not_capped` | `src/cc/hpcc.rs` | EWMA weight tau/T may exceed 1 (Algorithm 1 line 8 dropped) | killed | cc::hpcc::tests::ewma_weight_is_capped_at_one_rtt, golden_digest |
| `pfc_pause_off_by_one` | `src/mmu.rs` | PFC pause triggers one byte late | killed | mmu::tests::pause_and_resume_boundaries |
| `pfc_resume_off_by_one` | `src/mmu.rs` | PFC resume hysteresis off by one byte | killed | mmu::tests::pause_and_resume_boundaries |
| `mmu_shared_leak` | `src/mmu.rs` | shared-buffer accounting releases reserved bytes too | killed | a_sending_window_stops_dcqcn_from_triggering_pfc, byte_conservation_randomised, golden_digest, lone_flow_fct_equals_exact_ideal_everywhere, mmu::tests::randomised_accounting_invariants, pfc_prevents_all_drops_under_line_rate_incast, pfc_propagates_hop_by_hop_without_loss, without_pfc_the_same_incast_drops_and_go_back_n_recovers |
| `ecn_k_wrong_scale` | `src/sim.rs` | DCTCP marking threshold not scaled with link speed | killed | dctcp_holds_the_queue_near_its_marking_threshold, golden_digest |
| `serialization_units` | `src/time.rs` | per-byte serialization time computed in bytes instead of bits | killed | byte_conservation_randomised, golden_digest, hpcc_drains_incast_in_a_few_rtts_without_overreacting, hpcc_steady_state_queue_is_near_zero, lone_flow_fct_equals_exact_ideal_everywhere, pfc_propagates_hop_by_hop_without_loss, single_packet_latency_is_analytic, time::tests::serialization_is_exact_for_paper_rates |
| `event_ties_lifo` | `src/event.rs` | simultaneous events run in reverse order | killed | event::tests::equal_times_are_fifo, event::tests::randomised_against_stable_sort, golden_digest |
| `ecmp_ignores_dport` | `src/routing.rs` | ECMP hash ignores the destination port | killed | golden_digest, routing::tests::ecmp_hash_uses_all_tuple_fields |
| `dcqcn_alpha_sign` | `src/cc/dcqcn.rs` | DCQCN alpha decreases when CNPs arrive | killed | cc::dcqcn::tests::alpha_rises_while_cnps_keep_arriving, golden_digest |
| `drops_not_counted` | `src/sim.rs` | switch drops vanish from the byte accounting | killed | byte_conservation_randomised, without_pfc_the_same_incast_drops_and_go_back_n_recovers |
| `no_pacing` | `src/sim.rs` | NIC ignores the CC's pacing rate | killed | golden_digest, hpcc_drains_incast_in_a_few_rtts_without_overreacting |
| `ideal_no_ack_serialization` | `src/sim.rs` | ideal FCT forgets to serialize the ACK on the way back | killed | golden_digest, ideal_matches_closed_form_for_uniform_path, lone_flow_fct_equals_exact_ideal_everywhere |
| `int_qlen_before_dequeue` | `src/sim.rs` | switches report an empty queue in INT | killed | golden_digest, hpcc_drains_incast_in_a_few_rtts_without_overreacting, hpcc_reacts_to_the_last_hop_of_a_multi_hop_path |

16 of 16 mutants killed. `golden_digest` fails on any change of simulated behaviour, so it is not counted as a targeted kill; every mutant is also killed by at least one targeted test.
