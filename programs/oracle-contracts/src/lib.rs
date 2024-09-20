use anchor_lang::prelude::*;
use anchor_lang::solana_program::hash::hash;

declare_id!("CyJDfKuJ7aAF86dJifrKXBWLLrT2TcmoqSVvqgTJ9FR6");

#[program]
pub mod binary_oracle {
    use super::*;

    pub fn initialize(
        ctx: Context<Initialize>, 
        collateral: u64, 
        reveal_duration: i64, 
        max_nodes: u64
    ) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        oracle.authority = ctx.accounts.authority.key();
        oracle.collateral = collateral;
        oracle.is_resolved = false;
        oracle.resolution_bit = false;
        oracle.phase = Phase::Precommit;
        oracle.reveal_end_time = 0;
        oracle.reveal_duration = reveal_duration;
        oracle.max_nodes = max_nodes;
        oracle.total_nodes = 0;
        oracle.committed_nodes = 0;
        Ok(())
    }

    //join network during precommit or commit phase, post collateral
    pub fn join_network(ctx: Context<JoinNetwork>) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        let node = &mut ctx.accounts.node;
        let node_authority = &ctx.accounts.node_authority;

        require!(
            oracle.phase == Phase::Precommit || oracle.phase == Phase::Commit,
            ErrorCode::InvalidPhaseForJoining
        );
        require!(
            oracle.total_nodes < oracle.max_nodes,
            ErrorCode::MaxNodesReached
        );

        // Transfer collateral from node authority to oracle account
        let collateral = oracle.collateral;
        **node_authority.to_account_info().try_borrow_mut_lamports()? -= collateral;
        **oracle.to_account_info().try_borrow_mut_lamports()? += collateral;

        node.authority = node_authority.key();
        node.vote_hash = None;
        node.vote = None;
        node.slashed = false;

        oracle.total_nodes += 1;

        Ok(())
    }

    //start the request (must be oracle authority)
    pub fn start_request(ctx: Context<StartRequest>) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        require!(oracle.phase == Phase::Precommit, ErrorCode::InvalidPhase);
        require!(
            ctx.accounts.authority.key() == oracle.authority,
            ErrorCode::UnauthorizedAccess
        );

        oracle.phase = Phase::Commit;
        oracle.committed_nodes = 0;

        Ok(())
    }

    //commit vote during commit phase
    pub fn commit(ctx: Context<Commit>, vote_hash: [u8; 32]) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        let node = &mut ctx.accounts.node;

        require!(oracle.phase == Phase::Commit, ErrorCode::InvalidPhase);
        require!(node.vote_hash.is_none(), ErrorCode::AlreadyCommitted);

        node.vote_hash = Some(vote_hash);
        oracle.committed_nodes += 1;

        // If all nodes have committed, start the reveal phase
        if oracle.committed_nodes == oracle.total_nodes {
            oracle.phase = Phase::Reveal;
            let clock = Clock::get()?;
            oracle.reveal_end_time = clock.unix_timestamp + oracle.reveal_duration;
        }

        Ok(())
    }

    //reveal vote during reveal phase
    pub fn reveal(ctx: Context<Reveal>, vote: bool, nonce: [u8; 32]) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        let node = &mut ctx.accounts.node;

        require!(oracle.phase == Phase::Reveal, ErrorCode::InvalidPhase);
        require!(Clock::get()?.unix_timestamp <= oracle.reveal_end_time, ErrorCode::RevealPhaseClosed);
        require!(node.vote_hash.is_some(), ErrorCode::NotCommitted);
        require!(node.vote.is_none(), ErrorCode::AlreadyRevealed);

        let calculated_hash = hash(&[&[vote as u8], &nonce[..]].concat()).to_bytes();
        require!(calculated_hash == node.vote_hash.unwrap(), ErrorCode::InvalidReveal);

        node.vote = Some(vote);

        Ok(())
    }

    //slash colluding node with proof of collusion
    pub fn slash_colluding(ctx: Context<SlashColluding>, vote: bool, nonce: [u8; 32]) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        let colluding_node = &mut ctx.accounts.colluding_node;

        require!(oracle.phase == Phase::Commit, ErrorCode::InvalidPhase);
        require!(colluding_node.vote_hash.is_some(), ErrorCode::NotCommitted);

        let calculated_hash = hash(&[&[vote as u8], &nonce[..]].concat()).to_bytes();
        require!(calculated_hash == colluding_node.vote_hash.unwrap(), ErrorCode::InvalidCollusion);

        // Transfer collateral from colluding node to oracle pool
        let collateral = oracle.collateral;
        **colluding_node.to_account_info().try_borrow_mut_lamports()? -= collateral;
        **oracle.to_account_info().try_borrow_mut_lamports()? += collateral;

        colluding_node.slashed = true;
        
        emit!(NodeSlashed { 
            oracle: oracle.key(), 
            slashed_node: colluding_node.key() 
        });

        Ok(())
    }

    //resolves the request, distributes slashed collateral to consensus nodes
    pub fn resolve<'info>(
        ctx: Context<'_, '_, 'info, 'info, Resolve<'info>>
    ) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        require!(oracle.phase == Phase::Reveal, ErrorCode::InvalidPhase);
        require!(Clock::get()?.unix_timestamp > oracle.reveal_end_time, ErrorCode::RevealPhaseNotClosed);

        let mut true_votes = 0;
        let mut false_votes = 0;
        let mut total_nodes = 0;

        for node_info in ctx.remaining_accounts.iter() {
            let node = Account::<Node>::try_from(node_info)?;
            if !node.slashed {
                total_nodes += 1;
                if let Some(vote) = node.vote {
                    if vote {
                        true_votes += 1;
                    } else {
                        false_votes += 1;
                    }
                }
            }
        }

        oracle.is_resolved = true;
        oracle.resolution_bit = true_votes > false_votes;
        let consensus_nodes = if oracle.resolution_bit { true_votes } else { false_votes };

        // Distribute rewards to consensus nodes
        let reward_per_node = if consensus_nodes > 0 {
            oracle.collateral * total_nodes / consensus_nodes
        } else {
            0
        };

        for node_info in ctx.remaining_accounts.iter() {
            let node = Account::<Node>::try_from(node_info)?;
            if !node.slashed && node.vote == Some(oracle.resolution_bit) {
                **node_info.try_borrow_mut_lamports()? += reward_per_node;
                **oracle.to_account_info().try_borrow_mut_lamports()? -= reward_per_node;
            }
        }

        oracle.phase = Phase::Complete;

        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Precommit,
    Commit,
    Reveal,
    Complete,
}

#[account]
pub struct Oracle {
    pub authority: Pubkey,
    pub collateral: u64,
    pub is_resolved: bool,
    pub resolution_bit: bool,
    pub phase: Phase,
    pub reveal_end_time: i64,
    pub reveal_duration: i64,
    pub max_nodes: u64,
    pub total_nodes: u64,
    pub committed_nodes: u64,
}

#[account]
pub struct Node {
    pub authority: Pubkey,
    pub vote_hash: Option<[u8; 32]>,
    pub vote: Option<bool>,
    pub slashed: bool,
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init, payer = authority, space = 8 + 32 + 8 + 1 + 1 + 1 + 8 + 8 + 8 + 8 + 8)]
    pub oracle: Account<'info, Oracle>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct JoinNetwork<'info> {
    #[account(mut)]
    pub oracle: Account<'info, Oracle>,
    #[account(init, payer = node_authority, space = 8 + 32 + 33 + 2 + 1)]
    pub node: Account<'info, Node>,
    #[account(mut)]
    pub node_authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct StartRequest<'info> {
    #[account(mut)]
    pub oracle: Account<'info, Oracle>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct Commit<'info> {
    #[account(mut)]
    pub oracle: Account<'info, Oracle>,
    #[account(mut, has_one = authority)]
    pub node: Account<'info, Node>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct Reveal<'info> {
    #[account(mut)]
    pub oracle: Account<'info, Oracle>,
    #[account(mut, has_one = authority)]
    pub node: Account<'info, Node>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct SlashColluding<'info> {
    #[account(mut)]
    pub oracle: Account<'info, Oracle>,
    #[account(mut)]
    pub colluding_node: Account<'info, Node>,
    pub slasher: Signer<'info>,
}

#[derive(Accounts)]
pub struct Resolve<'info> {
    #[account(mut)]
    pub oracle: Account<'info, Oracle>,
    pub authority: Signer<'info>,
}

#[event]
pub struct NodeSlashed {
    pub oracle: Pubkey,
    pub slashed_node: Pubkey,
}

#[error_code]
pub enum ErrorCode {
    #[msg("Invalid phase for this operation")]
    InvalidPhase,
    #[msg("Reveal phase is closed")]
    RevealPhaseClosed,
    #[msg("Invalid reveal")]
    InvalidReveal,
    #[msg("Invalid collusion proof")]
    InvalidCollusion,
    #[msg("Reveal phase is not closed yet")]
    RevealPhaseNotClosed,
    #[msg("Node has not committed")]
    NotCommitted,
    #[msg("Node has already committed")]
    AlreadyCommitted,
    #[msg("Node has already revealed")]
    AlreadyRevealed,
    #[msg("Invalid phase for joining the network")]
    InvalidPhaseForJoining,
    #[msg("Maximum number of nodes reached")]
    MaxNodesReached,
    #[msg("Unauthorized access")]
    UnauthorizedAccess,
}