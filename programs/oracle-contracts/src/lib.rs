use anchor_lang::prelude::*;
use anchor_lang::solana_program::hash::{hash, Hash};

declare_id!("CyJDfKuJ7aAF86dJifrKXBWLLrT2TcmoqSVvqgTJ9FR6");

#[program]
pub mod binary_oracle {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, collateral: u64) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        oracle.authority = ctx.accounts.authority.key();
        oracle.collateral = collateral;
        oracle.is_resolved = false;
        oracle.resolution_bit = false;
        oracle.phase = Phase::Inactive;
        oracle.reveal_end_time = 0;
        oracle.total_nodes = 0;
        oracle.committed_nodes = 0;
        Ok(())
    }

    pub fn join_network(ctx: Context<JoinNetwork>) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        let node = &mut ctx.accounts.node;
        let node_authority = &ctx.accounts.node_authority;

        // Transfer collateral from node authority to oracle account
        let collateral = oracle.collateral;
        **node_authority.to_account_info().try_borrow_mut_lamports()? -= collateral;
        **oracle.to_account_info().try_borrow_mut_lamports()? += collateral;

        node.authority = node_authority.key();
        node.joined = true;
        node.slashed = false;

        oracle.total_nodes += 1;

        Ok(())
    }

    pub fn start_request(ctx: Context<StartRequest>, reveal_duration: i64) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        require!(oracle.phase == Phase::Inactive, ErrorCode::InvalidPhase);

        oracle.phase = Phase::Commit;
        oracle.committed_nodes = 0;
        oracle.reveal_duration = reveal_duration;

        Ok(())
    }

    pub fn commit(ctx: Context<Commit>, vote_hash: [u8; 32]) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        let node = &mut ctx.accounts.node;

        require!(oracle.phase == Phase::Commit, ErrorCode::InvalidPhase);
        require!(node.joined, ErrorCode::NodeNotJoined);
        require!(!node.committed, ErrorCode::AlreadyCommitted);

        node.vote_hash = vote_hash;
        node.committed = true;
        node.revealed = false;

        oracle.committed_nodes += 1;

        // If all nodes have committed, start the reveal phase
        if oracle.committed_nodes == oracle.total_nodes {
            oracle.phase = Phase::Reveal;
            let clock = Clock::get()?;
            oracle.reveal_end_time = clock.unix_timestamp + oracle.reveal_duration;
        }

        Ok(())
    }

    pub fn reveal(ctx: Context<Reveal>, vote: bool, nonce: [u8; 32]) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        let node = &mut ctx.accounts.node;

        require!(oracle.phase == Phase::Reveal, ErrorCode::InvalidPhase);
        require!(Clock::get()?.unix_timestamp <= oracle.reveal_end_time, ErrorCode::RevealPhaseClosed);

        let calculated_hash = hash(&[&[vote as u8], &nonce[..]].concat()).to_bytes();
        require!(calculated_hash == node.vote_hash, ErrorCode::InvalidReveal);

        node.vote = vote;
        node.revealed = true;

        Ok(())
    }

    pub fn slash_colluding(ctx: Context<SlashColluding>, vote: bool, nonce: [u8; 32]) -> Result<()> {
        let oracle = &ctx.accounts.oracle;
        let colluding_node = &mut ctx.accounts.colluding_node;
        let slasher_node = &mut ctx.accounts.slasher_node;

        require!(oracle.phase == Phase::Commit, ErrorCode::InvalidPhase);

        let calculated_hash = hash(&[&[vote as u8], &nonce[..]].concat()).to_bytes();
        require!(calculated_hash == colluding_node.vote_hash, ErrorCode::InvalidCollusion);

        // Transfer collateral from colluding node to slasher
        let collateral = oracle.collateral;
        **colluding_node.to_account_info().try_borrow_mut_lamports()? -= collateral;
        **slasher_node.to_account_info().try_borrow_mut_lamports()? += collateral;

        colluding_node.slashed = true;
        
        // Update total_nodes in a separate instruction
        emit!(NodeSlashed { 
            oracle: oracle.key(), 
            slashed_node: colluding_node.key() 
        });

        Ok(())
    }

    pub fn resolve<'info>(
        ctx: Context<'_, '_, 'info, 'info, Resolve<'info>>
    ) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        require!(oracle.phase == Phase::Reveal, ErrorCode::InvalidPhase);
        require!(Clock::get()?.unix_timestamp > oracle.reveal_end_time, ErrorCode::RevealPhaseNotClosed);

        let mut true_votes = 0;
        let mut false_votes = 0;
        let mut total_nodes = 0;
        let mut consensus_nodes = 0;

        let nodes: Vec<(&'info AccountInfo<'info>, Account<'info, Node>)> = ctx.remaining_accounts
            .iter()
            .filter_map(|account_info| {
                match Account::<Node>::try_from(account_info) {
                    Ok(node) => Some((account_info, node)),
                    Err(_) => None,
                }
            })
            .collect();

        for (account_info, node) in nodes.iter() {
            if !node.slashed && node.joined {
                total_nodes += 1;
                if node.revealed {
                    if node.vote {
                        true_votes += 1;
                    } else {
                        false_votes += 1;
                    }
                } else {
                    // Slash unrevealed nodes
                    **account_info.try_borrow_mut_lamports()? -= oracle.collateral;
                    **oracle.to_account_info().try_borrow_mut_lamports()? += oracle.collateral;
                }
            }
        }

        oracle.is_resolved = true;
        oracle.resolution_bit = true_votes > false_votes;
        consensus_nodes = if oracle.resolution_bit { true_votes } else { false_votes };

        // Distribute slashed collateral to consensus nodes
        let reward_per_node = if consensus_nodes > 0 {
            (total_nodes - consensus_nodes) * oracle.collateral / consensus_nodes
        } else {
            0
        };

        for (account_info, node) in nodes.iter() {
            if !node.slashed && node.joined && node.revealed && node.vote == oracle.resolution_bit {
                **account_info.try_borrow_mut_lamports()? += reward_per_node;
                **oracle.to_account_info().try_borrow_mut_lamports()? -= reward_per_node;
            }
        }

        oracle.phase = Phase::Inactive;

        Ok(())
    }

}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Inactive,
    Commit,
    Reveal,
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
    pub total_nodes: u64,
    pub committed_nodes: u64,
}

#[account]
pub struct Node {
    pub authority: Pubkey,
    pub joined: bool,
    pub vote_hash: [u8; 32],
    pub vote: bool,
    pub committed: bool,
    pub revealed: bool,
    pub slashed: bool,
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init, payer = authority, space = 8 + 32 + 8 + 1 + 1 + 1 + 8 + 8 + 8 + 8)]
    pub oracle: Account<'info, Oracle>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct JoinNetwork<'info> {
    #[account(mut)]
    pub oracle: Account<'info, Oracle>,
    #[account(init, payer = node_authority, space = 8 + 32 + 1 + 32 + 1 + 1 + 1 + 1)]
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
    #[account(mut)]
    pub slasher_node: Account<'info, Node>,
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
    #[msg("Node has not joined the network")]
    NodeNotJoined,
    #[msg("Node has already committed")]
    AlreadyCommitted,
}