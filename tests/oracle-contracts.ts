import * as anchor from "@project-serum/anchor";
import { Program } from "@project-serum/anchor";
import { BinaryOracle } from "../target/types/binary_oracle";
import { expect } from "chai";

describe("binary-oracle", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.BinaryOracle as Program<BinaryOracle>;

  let oracle: anchor.web3.Keypair;
  let node1: anchor.web3.Keypair;
  let node2: anchor.web3.Keypair;
  let node3: anchor.web3.Keypair;

  const COLLATERAL = new anchor.BN(1_000_000_000); // 1 SOL
  const REVEAL_DURATION = 100; // 100 seconds

  before(async () => {
    oracle = anchor.web3.Keypair.generate();
    node1 = anchor.web3.Keypair.generate();
    node2 = anchor.web3.Keypair.generate();
    node3 = anchor.web3.Keypair.generate();

    // Airdrop SOL to nodes
    await provider.connection.requestAirdrop(node1.publicKey, 10_000_000_000);
    await provider.connection.requestAirdrop(node2.publicKey, 10_000_000_000);
    await provider.connection.requestAirdrop(node3.publicKey, 10_000_000_000);

    // Initialize oracle
    await program.methods
      .initialize(COLLATERAL)
      .accounts({
        oracle: oracle.publicKey,
        authority: provider.wallet.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([oracle])
      .rpc();
  });

  it("1. Ensures that nodes joining the oracle must post a collateral", async () => {
    const nodeAccount = anchor.web3.Keypair.generate();
    const tx = program.methods
      .joinNetwork()
      .accounts({
        oracle: oracle.publicKey,
        node: nodeAccount.publicKey,
        nodeAuthority: node1.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([nodeAccount, node1]);

    await expect(tx.rpc()).to.not.be.rejected;

    const nodeState = await program.account.node.fetch(nodeAccount.publicKey);
    expect(nodeState.joined).to.be.true;

    const oracleBalance = await provider.connection.getBalance(oracle.publicKey);
    expect(oracleBalance).to.equal(COLLATERAL.toNumber());
  });

  it("2. Ensures that a node cannot commit without having joined with a collateral", async () => {
    const nonJoinedNode = anchor.web3.Keypair.generate();
    const voteHash = anchor.web3.Keypair.generate().publicKey.toBytes();

    await program.methods.startRequest(new anchor.BN(REVEAL_DURATION)).accounts({
      oracle: oracle.publicKey,
      authority: provider.wallet.publicKey,
    }).rpc();

    const tx = program.methods
      .commit(voteHash)
      .accounts({
        oracle: oracle.publicKey,
        node: nonJoinedNode.publicKey,
        authority: nonJoinedNode.publicKey,
      })
      .signers([nonJoinedNode]);

    await expect(tx.rpc()).to.be.rejectedWith(/NodeNotJoined/);
  });

  it("3. Ensures that a node cannot commit during reveal phase", async () => {
    // Join node2
    const node2Account = anchor.web3.Keypair.generate();
    await program.methods
      .joinNetwork()
      .accounts({
        oracle: oracle.publicKey,
        node: node2Account.publicKey,
        nodeAuthority: node2.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([node2Account, node2])
      .rpc();

    // Commit for node1 (joined in test 1)
    const voteHash1 = anchor.web3.Keypair.generate().publicKey.toBytes();
    await program.methods
      .commit(voteHash1)
      .accounts({
        oracle: oracle.publicKey,
        node: node1.publicKey,
        authority: node1.publicKey,
      })
      .signers([node1])
      .rpc();

    // Commit for node2
    const voteHash2 = anchor.web3.Keypair.generate().publicKey.toBytes();
    await program.methods
      .commit(voteHash2)
      .accounts({
        oracle: oracle.publicKey,
        node: node2Account.publicKey,
        authority: node2.publicKey,
      })
      .signers([node2])
      .rpc();

    // Try to commit again (should fail as we're in reveal phase)
    const tx = program.methods
      .commit(voteHash2)
      .accounts({
        oracle: oracle.publicKey,
        node: node2Account.publicKey,
        authority: node2.publicKey,
      })
      .signers([node2]);

    await expect(tx.rpc()).to.be.rejectedWith(/InvalidPhase/);
  });

  it("4. Ensures that a node can slash another node and receive their collateral with the correct nonce", async () => {
    // Join node3
    const node3Account = anchor.web3.Keypair.generate();
    await program.methods
      .joinNetwork()
      .accounts({
        oracle: oracle.publicKey,
        node: node3Account.publicKey,
        nodeAuthority: node3.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([node3Account, node3])
      .rpc();

    // Start a new request
    await program.methods.startRequest(new anchor.BN(REVEAL_DURATION)).accounts({
      oracle: oracle.publicKey,
      authority: provider.wallet.publicKey,
    }).rpc();

    // Node3 commits with a known vote and nonce
    const vote = true;
    const nonce = anchor.web3.Keypair.generate().publicKey.toBytes();
    const voteHash = await program.methods.calculateHash(vote, nonce).view();

    await program.methods
      .commit(voteHash)
      .accounts({
        oracle: oracle.publicKey,
        node: node3Account.publicKey,
        authority: node3.publicKey,
      })
      .signers([node3])
      .rpc();

    // Node2 slashes Node3
    const balanceBefore = await provider.connection.getBalance(node2.publicKey);

    await program.methods
      .slashColluding(vote, nonce)
      .accounts({
        oracle: oracle.publicKey,
        colludingNode: node3Account.publicKey,
        slasherNode: node2Account.publicKey,
        slasher: node2.publicKey,
      })
      .signers([node2])
      .rpc();

    const balanceAfter = await provider.connection.getBalance(node2.publicKey);
    expect(balanceAfter - balanceBefore).to.equal(COLLATERAL.toNumber());

    const slashedNode = await program.account.node.fetch(node3Account.publicKey);
    expect(slashedNode.slashed).to.be.true;
  });

  it("5. Ensures that a node cannot reveal after time t has passed", async () => {
    // Wait for reveal phase to end
    await new Promise(resolve => setTimeout(resolve, (REVEAL_DURATION + 1) * 1000));

    const vote = true;
    const nonce = anchor.web3.Keypair.generate().publicKey.toBytes();

    const tx = program.methods
      .reveal(vote, nonce)
      .accounts({
        oracle: oracle.publicKey,
        node: node1.publicKey,
        authority: node1.publicKey,
      })
      .signers([node1]);

    await expect(tx.rpc()).to.be.rejectedWith(/RevealPhaseClosed/);
  });

  it("6&7. Ensures that nodes that didn't vote the consensus get slashed and consensus nodes split the collateral", async () => {
    // Start a new request
    await program.methods.startRequest(new anchor.BN(REVEAL_DURATION)).accounts({
      oracle: oracle.publicKey,
      authority: provider.wallet.publicKey,
    }).rpc();

    // Node1 and Node2 commit and reveal with different votes
    const vote1 = true;
    const nonce1 = anchor.web3.Keypair.generate().publicKey.toBytes();
    const voteHash1 = await program.methods.calculateHash(vote1, nonce1).view();

    const vote2 = false;
    const nonce2 = anchor.web3.Keypair.generate().publicKey.toBytes();
    const voteHash2 = await program.methods.calculateHash(vote2, nonce2).view();

    await program.methods
      .commit(voteHash1)
      .accounts({
        oracle: oracle.publicKey,
        node: node1.publicKey,
        authority: node1.publicKey,
      })
      .signers([node1])
      .rpc();

    await program.methods
      .commit(voteHash2)
      .accounts({
        oracle: oracle.publicKey,
        node: node2Account.publicKey,
        authority: node2.publicKey,
      })
      .signers([node2])
      .rpc();

    // Reveal votes
    await program.methods
      .reveal(vote1, nonce1)
      .accounts({
        oracle: oracle.publicKey,
        node: node1.publicKey,
        authority: node1.publicKey,
      })
      .signers([node1])
      .rpc();

    await program.methods
      .reveal(vote2, nonce2)
      .accounts({
        oracle: oracle.publicKey,
        node: node2Account.publicKey,
        authority: node2.publicKey,
      })
      .signers([node2])
      .rpc();

    // Wait for reveal phase to end
    await new Promise(resolve => setTimeout(resolve, (REVEAL_DURATION + 1) * 1000));

    // Resolve the oracle
    const balanceBefore1 = await provider.connection.getBalance(node1.publicKey);
    const balanceBefore2 = await provider.connection.getBalance(node2.publicKey);

    await program.methods
      .resolve()
      .accounts({
        oracle: oracle.publicKey,
        authority: provider.wallet.publicKey,
      })
      .remainingAccounts([
        { pubkey: node1.publicKey, isWritable: true, isSigner: false },
        { pubkey: node2Account.publicKey, isWritable: true, isSigner: false },
      ])
      .rpc();

    const balanceAfter1 = await provider.connection.getBalance(node1.publicKey);
    const balanceAfter2 = await provider.connection.getBalance(node2.publicKey);

    // Check that the consensus node (Node1) received the collateral from the non-consensus node (Node2)
    expect(balanceAfter1 - balanceBefore1).to.equal(COLLATERAL.toNumber());
    expect(balanceBefore2 - balanceAfter2).to.equal(COLLATERAL.toNumber());

    const oracleState = await program.account.oracle.fetch(oracle.publicKey);
    expect(oracleState.resolutionBit).to.equal(vote1);
    expect(oracleState.isResolved).to.be.true;
  });
});