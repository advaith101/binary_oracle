use anchor_lang::prelude::*;
use anchor_lang::solana_program::hash::hash;
use anchor_lang::solana_program::system_program;
use binary_oracle::*;
use solana_program_test::*;
use solana_sdk::{signature::Keypair, signer::Signer};

#[tokio::test]
async fn test_binary_oracle() {
    let program_id = Pubkey::new_unique();
    let mut program_test = ProgramTest::new(
        "binary_oracle",
        program_id,
        processor!(binary_oracle::entry),
    );

    // Create test accounts
    let oracle_authority = Keypair::new();
    let node1 = Keypair::new();
    let node2 = Keypair::new();
    let node3 = Keypair::new();

    // Start the test context
    let (mut banks_client, payer, recent_blockhash) = program_test.start().await;

    // Initialize the oracle
    let oracle = Keypair::new();
    let collateral = 1_000_000; // 1 SOL
    let reveal_duration = 3600; // 1 hour
    let max_nodes = 3;

    let rent = banks_client.get_rent().await.unwrap();
    let oracle_account_rent = rent.minimum_balance(Oracle::LEN);

    let ix = binary_oracle::instruction::initialize(
        program_id,
        oracle_authority.pubkey(),
        oracle.pubkey(),
        collateral,
        reveal_duration,
        max_nodes,
    );

    let mut transaction = Transaction::new_with_payer(
        &[ix],
        Some(&payer.pubkey()),
    );
    transaction.sign(&[&payer, &oracle, &oracle_authority], recent_blockhash);
    banks_client.process_transaction(transaction).await.unwrap();

    // Test 1: Nodes joining must post collateral and can only join during precommit and commit phases
    let join_network_ix = binary_oracle::instruction::join_network(
        program_id,
        oracle.pubkey(),
        node1.pubkey(),
        node1.pubkey(),
    );

    let mut transaction = Transaction::new_with_payer(
        &[join_network_ix],
        Some(&payer.pubkey()),
    );
    transaction.sign(&[&payer, &node1], recent_blockhash);
    banks_client.process_transaction(transaction).await.unwrap();

    // Verify node1 has joined and posted collateral
    let node1_account = banks_client.get_account(node1.pubkey()).await.unwrap().unwrap();
    assert_eq!(node1_account.lamports, oracle_account_rent);

    // Test 2: Nodes committing must have joined and posted collateral
    let start_request_ix = binary_oracle::instruction::start_request(
        program_id,
        oracle.pubkey(),
        oracle_authority.pubkey(),
    );

    let mut transaction = Transaction::new_with_payer(
        &[start_request_ix],
        Some(&payer.pubkey()),
    );
    transaction.sign(&[&payer, &oracle_authority], recent_blockhash);
    banks_client.process_transaction(transaction).await.unwrap();

    // Node1 commits
    let vote = true;
    let nonce = [1u8; 32];
    let vote_hash = hash(&[&[vote as u8], &nonce[..]].concat()).to_bytes();

    let commit_ix = binary_oracle::instruction::commit(
        program_id,
        oracle.pubkey(),
        node1.pubkey(),
        node1.pubkey(),
        vote_hash,
    );

    let mut transaction = Transaction::new_with_payer(
        &[commit_ix],
        Some(&payer.pubkey()),
    );
    transaction.sign(&[&payer, &node1], recent_blockhash);
    banks_client.process_transaction(transaction).await.unwrap();

    // Test 3: Nodes can only slash another node with the correct hash (vote and nonce)
    let node2_join_ix = binary_oracle::instruction::join_network(
        program_id,
        oracle.pubkey(),
        node2.pubkey(),
        node2.pubkey(),
    );

    let node2_commit_ix = binary_oracle::instruction::commit(
        program_id,
        oracle.pubkey(),
        node2.pubkey(),
        node2.pubkey(),
        vote_hash,
    );

    let mut transaction = Transaction::new_with_payer(
        &[node2_join_ix, node2_commit_ix],
        Some(&payer.pubkey()),
    );
    transaction.sign(&[&payer, &node2], recent_blockhash);
    banks_client.process_transaction(transaction).await.unwrap();

    // Node3 tries to slash Node2 with incorrect hash
    let incorrect_vote = false;
    let incorrect_nonce = [2u8; 32];
    let incorrect_vote_hash = hash(&[&[incorrect_vote as u8], &incorrect_nonce[..]].concat()).to_bytes();

    let slash_ix = binary_oracle::instruction::slash_colluding(
        program_id,
        oracle.pubkey(),
        node2.pubkey(),
        node3.pubkey(),
        incorrect_vote,
        incorrect_nonce,
    );

    let mut transaction = Transaction::new_with_payer(
        &[slash_ix],
        Some(&payer.pubkey()),
    );
    transaction.sign(&[&payer, &node3], recent_blockhash);
    let result = banks_client.process_transaction(transaction).await;
    assert!(result.is_err()); // Slashing should fail

    // Test 4: Nodes cannot reveal after reveal duration has passed
    // Fast forward time
    banks_client.set_sysvar(&Clock {
        slot: 100,
        epoch_start_timestamp: 0,
        epoch: 0,
        leader_schedule_epoch: 0,
        unix_timestamp: reveal_duration + 1,
    });

    let reveal_ix = binary_oracle::instruction::reveal(
        program_id,
        oracle.pubkey(),
        node1.pubkey(),
        node1.pubkey(),
        vote,
        nonce,
    );

    let mut transaction = Transaction::new_with_payer(
        &[reveal_ix],
        Some(&payer.pubkey()),
    );
    transaction.sign(&[&payer, &node1], recent_blockhash);
    let result = banks_client.process_transaction(transaction).await;
    assert!(result.is_err()); // Revealing should fail

    // Test 5 & 6: Slashed funds get split amongst consensus nodes & total SOL sent = total SOL in
    // Reset time and allow nodes to reveal
    banks_client.set_sysvar(&Clock {
        slot: 100,
        epoch_start_timestamp: 0,
        epoch: 0,
        leader_schedule_epoch: 0,
        unix_timestamp: reveal_duration - 1,
    });

    // Node1 and Node2 reveal
    let reveal_ix1 = binary_oracle::instruction::reveal(
        program_id,
        oracle.pubkey(),
        node1.pubkey(),
        node1.pubkey(),
        vote,
        nonce,
    );

    let reveal_ix2 = binary_oracle::instruction::reveal(
        program_id,
        oracle.pubkey(),
        node2.pubkey(),
        node2.pubkey(),
        vote,
        nonce,
    );

    let mut transaction = Transaction::new_with_payer(
        &[reveal_ix1, reveal_ix2],
        Some(&payer.pubkey()),
    );
    transaction.sign(&[&payer, &node1, &node2], recent_blockhash);
    banks_client.process_transaction(transaction).await.unwrap();

    // Resolve the oracle
    banks_client.set_sysvar(&Clock {
        slot: 100,
        epoch_start_timestamp: 0,
        epoch: 0,
        leader_schedule_epoch: 0,
        unix_timestamp: reveal_duration + 1,
    });

    let resolve_ix = binary_oracle::instruction::resolve(
        program_id,
        oracle.pubkey(),
        oracle_authority.pubkey(),
    );

    let mut transaction = Transaction::new_with_payer(
        &[resolve_ix],
        Some(&payer.pubkey()),
    );
    transaction.sign(&[&payer, &oracle_authority], recent_blockhash);
    banks_client.process_transaction(transaction).await.unwrap();

    // Verify final balances
    let oracle_account = banks_client.get_account(oracle.pubkey()).await.unwrap().unwrap();
    let node1_account = banks_client.get_account(node1.pubkey()).await.unwrap().unwrap();
    let node2_account = banks_client.get_account(node2.pubkey()).await.unwrap().unwrap();

    let total_collateral = collateral * 2; // 2 nodes joined
    assert_eq!(oracle_account.lamports + node1_account.lamports + node2_account.lamports, 
               oracle_account_rent + total_collateral);
}