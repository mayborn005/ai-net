//! Storage-budget assertions for task creation and updates (Issue #771).

use contract_test_utils::{assert_storage_within, StorageBudget};
use soroban_sdk::{testutils::Address as _, Address, Bytes, BytesN, Env, Vec};
use task_store::{TaskStatus, TaskStoreContract, TaskStoreContractClient};

fn setup() -> (Env, TaskStoreContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let id = env.register(TaskStoreContract, ());
    let client = TaskStoreContractClient::new(&env, &id);
    client.initialize(&Address::generate(&env));
    (env, client)
}

fn task_id(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

fn store(env: &Env, client: &TaskStoreContractClient<'_>, submitter: &Address, task: &BytesN<32>) {
    let agents = Vec::from_array(env, [Address::generate(env)]);
    let dag = Bytes::from_slice(env, &[1, 2, 3]);
    client.store_task_metadata(
        submitter,
        task,
        &BytesN::from_array(env, &[9; 32]),
        &agents,
        &dag,
        &1,
        &None,
    );
}

/// A DAG task writes the metadata record, version history, version counter and
/// the creator's task index. Measured: +5 entries / +1,028 bytes. Ceiling:
/// 5 / 1,100.
#[test]
fn store_task_metadata_stays_within_storage_budget() {
    let (env, client) = setup();
    let submitter = Address::generate(&env);
    let task = task_id(&env, 1);

    assert_storage_within(
        &env,
        &client.address,
        "task_store::store_task_metadata",
        StorageBudget::new(5, 1_100),
        || store(&env, &client, &submitter, &task),
    );
}

/// A status update rewrites the record, appends one history record and bumps
/// the counter. The history vector grows in place, so the entry count is
/// unchanged. Measured: +0 entries / +176 bytes. Ceiling: 0 / 200.
#[test]
fn update_task_status_stays_within_storage_budget() {
    let (env, client) = setup();
    let submitter = Address::generate(&env);
    let task = task_id(&env, 2);
    store(&env, &client, &submitter, &task);

    assert_storage_within(
        &env,
        &client.address,
        "task_store::update_task_status",
        StorageBudget::new(0, 200),
        || client.update_task_status(&task, &submitter, &TaskStatus::Running),
    );
}

/// Lifecycle task creation writes the record, history and creator index.
/// Measured: +5 entries / +992 bytes. Ceiling: 5 / 1,100.
#[test]
fn create_task_stays_within_storage_budget() {
    let (env, client) = setup();
    let creator = Address::generate(&env);
    let task = task_id(&env, 3);

    assert_storage_within(
        &env,
        &client.address,
        "task_store::create_task",
        StorageBudget::new(5, 1_100),
        || {
            client.create_task(
                &task,
                &creator,
                &BytesN::from_array(&env, &[7; 32]),
                &1_000_000,
            )
        },
    );
}

/// Reads must not mutate storage.
#[test]
fn task_reads_are_storage_neutral() {
    let (env, client) = setup();
    let submitter = Address::generate(&env);
    let task = task_id(&env, 4);
    store(&env, &client, &submitter, &task);

    assert_storage_within(
        &env,
        &client.address,
        "task_store::get_task_metadata",
        StorageBudget::read_only(),
        || client.get_task_metadata(&task),
    );
    assert_storage_within(
        &env,
        &client.address,
        "task_store::get_tasks_by_creator",
        StorageBudget::read_only(),
        || client.get_tasks_by_creator(&submitter, &0, &10),
    );
}
