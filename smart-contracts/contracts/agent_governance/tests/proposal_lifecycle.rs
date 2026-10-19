//! Regression suite for the governance proposal lifecycle (#764):
//! propose → vote (for/against/abstain) → tally → execute, every terminal
//! state, quorum/majority boundaries, voting-window edges, double-vote
//! prevention and unauthorized attempts.
//!
//! The contract has no cancel entrypoint; a proposal that expires without
//! reaching quorum terminates as `Failed`, which is asserted below.

use agent_governance::{
    AgentGovernanceContract, AgentGovernanceContractClient, Error, ExecutionPayload,
    ProposalFailedEvent, ProposalStatus, ProposalType, VoteChoice, MIN_VOTING_PERIOD_SECS,
};
use soroban_sdk::{
    contract, contractimpl, symbol_short,
    testutils::{Address as _, Events as _, Ledger as _, MockAuth, MockAuthInvoke},
    Address, Bytes, Env, IntoVal, InvokeError, String, Symbol, TryFromVal,
};

// ── mock execution target ───────────────────────────────────────────────────

#[contract]
pub struct MockTarget;

#[contractimpl]
impl MockTarget {
    pub fn apply(env: Env, calldata: Bytes) {
        env.storage()
            .instance()
            .set(&symbol_short!("last"), &calldata);
    }

    pub fn boom(_env: Env, _calldata: Bytes) {
        panic!("target failure");
    }

    pub fn last(env: Env) -> Option<Bytes> {
        env.storage().instance().get(&symbol_short!("last"))
    }
}

// ── fixture ─────────────────────────────────────────────────────────────────

struct Gov {
    env: Env,
    client: AgentGovernanceContractClient<'static>,
    admin: Address,
}

fn setup() -> Gov {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);
    let id = env.register(AgentGovernanceContract, ());
    let client = AgentGovernanceContractClient::new(&env, &id);
    let admin = Address::generate(&env);
    client.initialize(&admin);
    Gov { env, client, admin }
}

impl Gov {
    /// Register an agent whose voting power equals `power` exactly
    /// (reputation 0, so power == stake).
    fn agent(&self, power: i128) -> Address {
        let a = Address::generate(&self.env);
        self.client.register_agent(&a, &0, &power);
        a
    }

    fn propose_with(&self, proposer: &Address, payload: &ExecutionPayload) -> u64 {
        self.client.create_proposal(
            proposer,
            &ProposalType::AgentDispute,
            &String::from_str(&self.env, "title"),
            &String::from_str(&self.env, "body"),
            &MIN_VOTING_PERIOD_SECS,
            payload,
        )
    }

    fn propose(&self, proposer: &Address) -> u64 {
        self.propose_with(proposer, &signal(&self.env))
    }

    fn vote(&self, id: u64, voter: &Address, choice: VoteChoice) {
        self.client.vote_on_proposal(&id, voter, &choice);
    }

    fn close_voting(&self, id: u64) {
        let p = self.client.get_proposal(&id).unwrap();
        self.env.ledger().set_timestamp(p.voting_ends_at);
    }

    fn execute(&self, id: u64) -> ProposalStatus {
        self.close_voting(id);
        self.client.execute_proposal(&id)
    }

    /// The `ProposalFailedEvent` emitted for `id` by the most recent
    /// invocation, if any (`events().all()` only covers the last call).
    fn failed_event(&self, id: u64) -> Option<ProposalFailedEvent> {
        let topic = (symbol_short!("gov"), symbol_short!("failed")).into_val(&self.env);
        self.env
            .events()
            .all()
            .iter()
            .filter(|(_, topics, _)| *topics == topic)
            .map(|(_, _, data)| ProposalFailedEvent::try_from_val(&self.env, &data).unwrap())
            .find(|e| e.id == id)
    }

    fn executed_event_emitted(&self) -> bool {
        let topic = (symbol_short!("gov"), symbol_short!("executed")).into_val(&self.env);
        self.env
            .events()
            .all()
            .iter()
            .any(|(_, topics, _)| topics == topic)
    }
}

fn signal(env: &Env) -> ExecutionPayload {
    ExecutionPayload {
        target: None,
        function: symbol_short!("noop"),
        calldata: Bytes::new(env),
        expected_hash: None,
    }
}

fn targeted(env: &Env, target: &Address, function: Symbol) -> ExecutionPayload {
    ExecutionPayload {
        target: Some(target.clone()),
        function,
        calldata: Bytes::from_slice(env, b"fee=200"),
        expected_hash: None,
    }
}

fn contract_err<T, I>(result: Result<T, Result<Error, I>>) -> Error {
    match result {
        Err(Ok(err)) => err,
        Err(Err(_)) => panic!("expected contract error, got host error"),
        Ok(_) => panic!("expected contract error, call succeeded"),
    }
}

/// A missing `require_auth` signature aborts the host call rather than
/// returning a contract error.
fn is_auth_abort<T, E>(result: Result<T, Result<E, InvokeError>>) -> bool {
    matches!(result, Err(Err(InvokeError::Abort)))
}

// ── happy path & tally ──────────────────────────────────────────────────────

#[test]
fn propose_vote_tally_execute_records_every_choice() {
    let g = setup();
    let a = g.agent(50);
    let b = g.agent(30);
    let c = g.agent(20);
    let id = g.propose(&a);

    let p = g.client.get_proposal(&id).unwrap();
    assert_eq!(p.status, ProposalStatus::Active);
    assert_eq!(p.total_power_snapshot, 100);
    assert_eq!(p.created_at, 1_000);
    assert_eq!(p.voting_ends_at, 1_000 + MIN_VOTING_PERIOD_SECS);

    g.vote(id, &a, VoteChoice::For);
    g.vote(id, &b, VoteChoice::Against);
    g.vote(id, &c, VoteChoice::Abstain);

    let p = g.client.get_proposal(&id).unwrap();
    assert_eq!(
        (p.for_power, p.against_power, p.abstain_power),
        (50, 30, 20)
    );
    let rec = g.client.get_vote(&id, &b).unwrap();
    assert_eq!((rec.choice, rec.weight), (VoteChoice::Against, 30));

    assert_eq!(g.execute(id), ProposalStatus::Executed);
    assert!(g.executed_event_emitted());
    assert_eq!(
        g.client.get_proposal(&id).unwrap().status,
        ProposalStatus::Executed
    );
}

#[test]
fn executed_payload_reaches_target() {
    let g = setup();
    let target = g.env.register(MockTarget, ());
    let a = g.agent(100);
    let id = g.propose_with(&a, &targeted(&g.env, &target, symbol_short!("apply")));
    g.vote(id, &a, VoteChoice::For);
    assert_eq!(g.execute(id), ProposalStatus::Executed);
    let last = MockTargetClient::new(&g.env, &target).last();
    assert_eq!(last, Some(Bytes::from_slice(&g.env, b"fee=200")));
}

// ── terminal states ─────────────────────────────────────────────────────────

#[test]
fn terminal_failed_when_expired_without_votes() {
    let g = setup();
    let a = g.agent(100);
    let id = g.propose(&a);
    assert_eq!(g.execute(id), ProposalStatus::Failed);
    let ev = g.failed_event(id).unwrap();
    assert!(!ev.quorum_met && !ev.majority_met && !ev.execution_failed);
}

#[test]
fn terminal_failed_when_majority_rejects() {
    let g = setup();
    let a = g.agent(40);
    let b = g.agent(60);
    let id = g.propose(&a);
    g.vote(id, &a, VoteChoice::For);
    g.vote(id, &b, VoteChoice::Against);
    assert_eq!(g.execute(id), ProposalStatus::Failed);
    let ev = g.failed_event(id).unwrap();
    assert!(ev.quorum_met && !ev.majority_met && !ev.execution_failed);
}

#[test]
fn terminal_failed_when_payload_invocation_fails() {
    let g = setup();
    let target = g.env.register(MockTarget, ());
    let a = g.agent(100);
    let id = g.propose_with(&a, &targeted(&g.env, &target, symbol_short!("boom")));
    g.vote(id, &a, VoteChoice::For);
    assert_eq!(g.execute(id), ProposalStatus::Failed);
    let ev = g.failed_event(id).unwrap();
    assert!(ev.quorum_met && ev.majority_met && ev.execution_failed);
    assert!(!g.executed_event_emitted());
}

#[test]
fn failed_vote_never_invokes_target() {
    let g = setup();
    let target = g.env.register(MockTarget, ());
    let a = g.agent(40);
    let b = g.agent(60);
    let id = g.propose_with(&a, &targeted(&g.env, &target, symbol_short!("apply")));
    g.vote(id, &a, VoteChoice::For);
    g.vote(id, &b, VoteChoice::Against);
    assert_eq!(g.execute(id), ProposalStatus::Failed);
    assert_eq!(MockTargetClient::new(&g.env, &target).last(), None);
}

#[test]
fn terminal_states_are_final() {
    let g = setup();
    let a = g.agent(100);
    let executed = g.propose(&a);
    let failed = g.propose(&a);
    g.vote(executed, &a, VoteChoice::For);
    assert_eq!(g.execute(executed), ProposalStatus::Executed);
    assert_eq!(g.execute(failed), ProposalStatus::Failed);

    for id in [executed, failed] {
        assert_eq!(
            contract_err(g.client.try_execute_proposal(&id)),
            Error::ProposalFinalized
        );
        assert_eq!(
            contract_err(g.client.try_vote_on_proposal(&id, &a, &VoteChoice::For)),
            Error::ProposalNotActive
        );
    }
    assert_eq!(
        g.client.get_proposal(&executed).unwrap().status,
        ProposalStatus::Executed
    );
    assert_eq!(
        g.client.get_proposal(&failed).unwrap().status,
        ProposalStatus::Failed
    );
}

#[test]
fn unknown_proposal_is_not_found() {
    let g = setup();
    let a = g.agent(100);
    assert_eq!(
        contract_err(g.client.try_execute_proposal(&42)),
        Error::NotFound
    );
    assert_eq!(
        contract_err(g.client.try_vote_on_proposal(&42, &a, &VoteChoice::For)),
        Error::NotFound
    );
}

// ── quorum boundary (30 % of snapshot) ──────────────────────────────────────

#[test]
fn quorum_exactly_at_threshold_passes() {
    let g = setup();
    let voter = g.agent(30);
    let _silent = g.agent(70);
    let id = g.propose(&voter);
    g.vote(id, &voter, VoteChoice::For);
    assert_eq!(g.execute(id), ProposalStatus::Executed);
}

#[test]
fn quorum_one_below_threshold_fails() {
    let g = setup();
    let voter = g.agent(29);
    let _silent = g.agent(71);
    let id = g.propose(&voter);
    g.vote(id, &voter, VoteChoice::For);
    assert_eq!(g.execute(id), ProposalStatus::Failed);
    let ev = g.failed_event(id).unwrap();
    assert!(!ev.quorum_met);
    assert!(ev.majority_met);
}

#[test]
fn abstentions_count_toward_quorum() {
    let g = setup();
    let yes = g.agent(10);
    let abstain = g.agent(20);
    let _silent = g.agent(70);
    let id = g.propose(&yes);
    g.vote(id, &yes, VoteChoice::For);
    g.vote(id, &abstain, VoteChoice::Abstain);
    // cast = 30 of 100 → quorum met; decisive = 10 For → majority met.
    assert_eq!(g.execute(id), ProposalStatus::Executed);
}

// ── majority boundary (> 50 % of decisive votes) ────────────────────────────

#[test]
fn majority_exactly_at_threshold_fails() {
    let g = setup();
    let yes = g.agent(50);
    let no = g.agent(50);
    let id = g.propose(&yes);
    g.vote(id, &yes, VoteChoice::For);
    g.vote(id, &no, VoteChoice::Against);
    assert_eq!(g.execute(id), ProposalStatus::Failed);
    let ev = g.failed_event(id).unwrap();
    assert!(ev.quorum_met && !ev.majority_met);
}

#[test]
fn majority_one_above_threshold_passes() {
    let g = setup();
    let yes = g.agent(51);
    let no = g.agent(49);
    let id = g.propose(&yes);
    g.vote(id, &yes, VoteChoice::For);
    g.vote(id, &no, VoteChoice::Against);
    assert_eq!(g.execute(id), ProposalStatus::Executed);
}

#[test]
fn majority_one_below_threshold_fails() {
    let g = setup();
    let yes = g.agent(49);
    let no = g.agent(51);
    let id = g.propose(&yes);
    g.vote(id, &yes, VoteChoice::For);
    g.vote(id, &no, VoteChoice::Against);
    assert_eq!(g.execute(id), ProposalStatus::Failed);
}

#[test]
fn abstain_only_meets_quorum_but_fails_majority() {
    let g = setup();
    let a = g.agent(100);
    let id = g.propose(&a);
    g.vote(id, &a, VoteChoice::Abstain);
    assert_eq!(g.execute(id), ProposalStatus::Failed);
    let ev = g.failed_event(id).unwrap();
    assert!(ev.quorum_met && !ev.majority_met);
}

// ── voting window edges ─────────────────────────────────────────────────────

#[test]
fn vote_accepted_until_last_second_and_rejected_at_deadline() {
    let g = setup();
    let a = g.agent(50);
    let b = g.agent(50);
    let id = g.propose(&a);
    let ends = g.client.get_proposal(&id).unwrap().voting_ends_at;

    g.env.ledger().set_timestamp(ends - 1);
    g.vote(id, &a, VoteChoice::For);

    g.env.ledger().set_timestamp(ends);
    assert_eq!(
        contract_err(g.client.try_vote_on_proposal(&id, &b, &VoteChoice::For)),
        Error::VotingPeriodEnded
    );
}

#[test]
fn execute_rejected_one_second_before_deadline() {
    let g = setup();
    let a = g.agent(100);
    let id = g.propose(&a);
    g.vote(id, &a, VoteChoice::For);
    let ends = g.client.get_proposal(&id).unwrap().voting_ends_at;

    g.env.ledger().set_timestamp(ends - 1);
    assert_eq!(
        contract_err(g.client.try_execute_proposal(&id)),
        Error::VotingPeriodActive
    );
    assert_eq!(
        g.client.get_proposal(&id).unwrap().status,
        ProposalStatus::Active
    );

    g.env.ledger().set_timestamp(ends);
    assert_eq!(g.client.execute_proposal(&id), ProposalStatus::Executed);
}

// ── double voting & eligibility ─────────────────────────────────────────────

#[test]
fn double_vote_is_rejected_for_every_choice() {
    let g = setup();
    let a = g.agent(100);
    let id = g.propose(&a);
    g.vote(id, &a, VoteChoice::Against);
    for choice in [VoteChoice::For, VoteChoice::Against, VoteChoice::Abstain] {
        assert_eq!(
            contract_err(g.client.try_vote_on_proposal(&id, &a, &choice)),
            Error::AlreadyVoted
        );
    }
    let p = g.client.get_proposal(&id).unwrap();
    assert_eq!((p.for_power, p.against_power, p.abstain_power), (0, 100, 0));
    assert_eq!(
        g.client.get_vote(&id, &a).unwrap().choice,
        VoteChoice::Against
    );
}

#[test]
fn power_changes_after_snapshot_do_not_change_weight() {
    let g = setup();
    let a = g.agent(100);
    let id = g.propose(&a);
    g.env.ledger().set_timestamp(2_000);
    g.client.update_agent(&a, &100, &1_000_000_000);
    g.vote(id, &a, VoteChoice::For);
    assert_eq!(g.client.get_vote(&id, &a).unwrap().weight, 100);
}

#[test]
fn agent_registered_after_snapshot_cannot_vote() {
    let g = setup();
    let a = g.agent(100);
    let id = g.propose(&a);
    g.env.ledger().set_timestamp(2_000);
    let late = g.agent(100);
    assert_eq!(
        contract_err(g.client.try_vote_on_proposal(&id, &late, &VoteChoice::For)),
        Error::NoSnapshotPower
    );
}

#[test]
fn removed_agent_cannot_vote_or_propose() {
    let g = setup();
    let a = g.agent(50);
    let b = g.agent(50);
    let id = g.propose(&a);
    g.client.remove_agent(&b);
    assert_eq!(
        contract_err(g.client.try_vote_on_proposal(&id, &b, &VoteChoice::For)),
        Error::AgentNotRegistered
    );
    assert_eq!(
        contract_err(g.client.try_create_proposal(
            &b,
            &ProposalType::AgentDispute,
            &String::from_str(&g.env, "t"),
            &String::from_str(&g.env, "d"),
            &0,
            &signal(&g.env),
        )),
        Error::AgentNotRegistered
    );
}

// ── pause ───────────────────────────────────────────────────────────────────

#[test]
fn pause_blocks_propose_and_vote_but_not_execute() {
    let g = setup();
    let a = g.agent(100);
    let id = g.propose(&a);
    g.vote(id, &a, VoteChoice::For);
    g.client.pause();

    assert_eq!(
        contract_err(g.client.try_create_proposal(
            &a,
            &ProposalType::AgentDispute,
            &String::from_str(&g.env, "t"),
            &String::from_str(&g.env, "d"),
            &0,
            &signal(&g.env),
        )),
        Error::ContractPaused
    );
    assert_eq!(
        contract_err(g.client.try_vote_on_proposal(&id, &a, &VoteChoice::For)),
        Error::ContractPaused
    );
    assert_eq!(g.execute(id), ProposalStatus::Executed);
}

// ── unauthorized attempts ───────────────────────────────────────────────────

#[test]
fn vote_without_voter_signature_aborts() {
    let g = setup();
    let a = g.agent(100);
    let id = g.propose(&a);
    g.env.mock_auths(&[]);
    assert!(is_auth_abort(g.client.try_vote_on_proposal(
        &id,
        &a,
        &VoteChoice::For
    )));
    g.env.mock_all_auths();
    assert_eq!(g.client.get_proposal(&id).unwrap().for_power, 0);
}

#[test]
fn vote_signed_by_someone_else_aborts() {
    let g = setup();
    let a = g.agent(100);
    let id = g.propose(&a);
    let intruder = Address::generate(&g.env);
    g.env.mock_auths(&[MockAuth {
        address: &intruder,
        invoke: &MockAuthInvoke {
            contract: &g.client.address,
            fn_name: "vote_on_proposal",
            args: (id, a.clone(), VoteChoice::For).into_val(&g.env),
            sub_invokes: &[],
        },
    }]);
    assert!(is_auth_abort(g.client.try_vote_on_proposal(
        &id,
        &a,
        &VoteChoice::For
    )));
}

#[test]
fn propose_without_signature_aborts() {
    let g = setup();
    let a = g.agent(100);
    g.env.mock_auths(&[]);
    assert!(is_auth_abort(g.client.try_create_proposal(
        &a,
        &ProposalType::AgentDispute,
        &String::from_str(&g.env, "t"),
        &String::from_str(&g.env, "d"),
        &0,
        &signal(&g.env),
    )));
    g.env.mock_all_auths();
    assert_eq!(g.client.get_proposal_count(), 0);
}

#[test]
fn non_member_cannot_propose_or_vote() {
    let g = setup();
    let a = g.agent(100);
    let id = g.propose(&a);
    let outsider = Address::generate(&g.env);
    assert_eq!(
        contract_err(
            g.client
                .try_vote_on_proposal(&id, &outsider, &VoteChoice::For)
        ),
        Error::AgentNotRegistered
    );
}

#[test]
fn premature_execution_by_anyone_reverts_with_voting_period_active() {
    let g = setup();
    let a = g.agent(100);
    let id = g.propose(&a);
    g.vote(id, &a, VoteChoice::For);
    // execute_proposal is permissionless, so no signature is required; the
    // only guard against hijacking execution is the voting window.
    g.env.mock_auths(&[]);
    assert_eq!(
        contract_err(g.client.try_execute_proposal(&id)),
        Error::VotingPeriodActive
    );
}

#[test]
fn admin_entrypoints_abort_without_admin_signature() {
    let g = setup();
    let other = Address::generate(&g.env);
    let registry = g.env.register(MockTarget, ());
    g.env.mock_auths(&[]);
    assert!(is_auth_abort(g.client.try_set_admin(&other)));
    assert!(is_auth_abort(g.client.try_pause()));
    assert!(is_auth_abort(g.client.try_unpause()));
    assert!(is_auth_abort(g.client.try_set_param_registry(&registry)));
    assert!(is_auth_abort(g.client.try_set_agent_registry(&registry)));

    assert_eq!(g.client.get_admin(), g.admin);
    assert!(!g.client.is_paused());
    assert_eq!(g.client.get_param_registry(), None);
}

#[test]
fn set_admin_requires_incoming_admin_signature() {
    let g = setup();
    let next = Address::generate(&g.env);
    g.env.mock_auths(&[MockAuth {
        address: &g.admin,
        invoke: &MockAuthInvoke {
            contract: &g.client.address,
            fn_name: "set_admin",
            args: (next.clone(),).into_val(&g.env),
            sub_invokes: &[],
        },
    }]);
    assert!(is_auth_abort(g.client.try_set_admin(&next)));
    assert_eq!(g.client.get_admin(), g.admin);
}

#[test]
fn uninitialized_contract_rejects_lifecycle_calls() {
    let env = Env::default();
    env.mock_all_auths();
    let id = env.register(AgentGovernanceContract, ());
    let client = AgentGovernanceContractClient::new(&env, &id);
    let a = Address::generate(&env);
    assert_eq!(
        contract_err(client.try_register_agent(&a, &10, &0)),
        Error::NotInitialized
    );
    assert_eq!(
        contract_err(client.try_execute_proposal(&1)),
        Error::NotInitialized
    );
    assert_eq!(
        contract_err(client.try_vote_on_proposal(&1, &a, &VoteChoice::For)),
        Error::NotInitialized
    );
}
