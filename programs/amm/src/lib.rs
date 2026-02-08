use anchor_lang::prelude::*;
use anchor_spl::token::{self, Burn, Mint, MintTo, Token, TokenAccount, Transfer};

declare_id!("Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS");

const FEE_BPS: u16 = 30; // 0.3%
const PROTOCOL_FEE_BPS: u16 = 5; // 0.05% to protocol, rest to LPs
const BPS_DENOMINATOR: u64 = 10_000;

#[program]
pub mod amm {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        require!(
            ctx.accounts.mint_a.key() != ctx.accounts.mint_b.key(),
            AmmError::SameMint
        );

        let pool = &mut ctx.accounts.pool;
        pool.mint_a = ctx.accounts.mint_a.key();
        pool.mint_b = ctx.accounts.mint_b.key();
        pool.vault_a = ctx.accounts.vault_a.key();
        pool.vault_b = ctx.accounts.vault_b.key();
        pool.lp_mint = ctx.accounts.lp_mint.key();
        pool.fee_vault_a = ctx.accounts.fee_vault_a.key();
        pool.fee_vault_b = ctx.accounts.fee_vault_b.key();
        pool.admin = ctx.accounts.admin.key();
        pool.bump = *ctx.bumps.get("pool").ok_or(AmmError::MissingBump)?;
        pool.fee_bps = FEE_BPS;
        pool.protocol_fee_bps = PROTOCOL_FEE_BPS;
        pool.paused = false;

        emit!(InitializeEvent {
            pool: pool.key(),
            mint_a: pool.mint_a,
            mint_b: pool.mint_b,
            lp_mint: pool.lp_mint,
            vault_a: pool.vault_a,
            vault_b: pool.vault_b,
            fee_vault_a: pool.fee_vault_a,
            fee_vault_b: pool.fee_vault_b,
            fee_bps: pool.fee_bps,
            protocol_fee_bps: pool.protocol_fee_bps,
            admin: pool.admin,
            paused: pool.paused,
        });
        Ok(())
    }

    pub fn deposit_liquidity(
        ctx: Context<DepositLiquidity>,
        amount_a: u64,
        amount_b: u64,
        min_lp_out: u64,
    ) -> Result<()> {
        require!(!ctx.accounts.pool.paused, AmmError::PoolPaused);
        require!(amount_a > 0 && amount_b > 0, AmmError::InvalidAmount);

        let reserve_a = ctx.accounts.vault_a.amount;
        let reserve_b = ctx.accounts.vault_b.amount;
        let total_lp = ctx.accounts.lp_mint.supply;

        let (used_a, used_b, lp_to_mint) = if total_lp == 0 {
            let product = (amount_a as u128)
                .checked_mul(amount_b as u128)
                .ok_or(AmmError::MathOverflow)?;
            let lp = integer_sqrt(product);
            (amount_a, amount_b, lp)
        } else {
            require!(reserve_a > 0 && reserve_b > 0, AmmError::InsufficientLiquidity);

            // Accept imbalanced deposits and mint LP from the limiting side.
            let ideal_b = (amount_a as u128)
                .checked_mul(reserve_b as u128)
                .ok_or(AmmError::MathOverflow)?
                .checked_div(reserve_a as u128)
                .ok_or(AmmError::MathOverflow)? as u64;
            if amount_b >= ideal_b {
                let lp = (amount_a as u128)
                    .checked_mul(total_lp as u128)
                    .ok_or(AmmError::MathOverflow)?
                    .checked_div(reserve_a as u128)
                    .ok_or(AmmError::MathOverflow)? as u64;
                (amount_a, ideal_b, lp)
            } else {
                let ideal_a = (amount_b as u128)
                    .checked_mul(reserve_a as u128)
                    .ok_or(AmmError::MathOverflow)?
                    .checked_div(reserve_b as u128)
                    .ok_or(AmmError::MathOverflow)? as u64;
                let lp = (amount_b as u128)
                    .checked_mul(total_lp as u128)
                    .ok_or(AmmError::MathOverflow)?
                    .checked_div(reserve_b as u128)
                    .ok_or(AmmError::MathOverflow)? as u64;
                (ideal_a, amount_b, lp)
            }
        };

        require!(lp_to_mint >= min_lp_out, AmmError::SlippageExceeded);

        token::transfer(ctx.accounts.transfer_to_vault_a_ctx(), used_a)?;
        token::transfer(ctx.accounts.transfer_to_vault_b_ctx(), used_b)?;

        let pool_seeds = ctx.accounts.pool.signer_seeds();
        token::mint_to(
            ctx.accounts.mint_lp_ctx().with_signer(&[&pool_seeds]),
            lp_to_mint,
        )?;

        emit!(DepositEvent {
            user: ctx.accounts.user.key(),
            pool: ctx.accounts.pool.key(),
            amount_a_in: used_a,
            amount_b_in: used_b,
            lp_minted: lp_to_mint,
        });

        Ok(())
    }

    pub fn withdraw_liquidity(
        ctx: Context<WithdrawLiquidity>,
        lp_amount: u64,
        min_amount_a: u64,
        min_amount_b: u64,
    ) -> Result<()> {
        require!(lp_amount > 0, AmmError::InvalidAmount);

        let reserve_a = ctx.accounts.vault_a.amount;
        let reserve_b = ctx.accounts.vault_b.amount;
        let total_lp = ctx.accounts.lp_mint.supply;
        require!(total_lp > 0, AmmError::InsufficientLiquidity);

        let amount_a = (lp_amount as u128)
            .checked_mul(reserve_a as u128)
            .ok_or(AmmError::MathOverflow)?
            .checked_div(total_lp as u128)
            .ok_or(AmmError::MathOverflow)? as u64;
        let amount_b = (lp_amount as u128)
            .checked_mul(reserve_b as u128)
            .ok_or(AmmError::MathOverflow)?
            .checked_div(total_lp as u128)
            .ok_or(AmmError::MathOverflow)? as u64;

        require!(amount_a >= min_amount_a, AmmError::SlippageExceeded);
        require!(amount_b >= min_amount_b, AmmError::SlippageExceeded);

        token::burn(ctx.accounts.burn_lp_ctx(), lp_amount)?;

        let pool_seeds = ctx.accounts.pool.signer_seeds();
        token::transfer(
            ctx.accounts.transfer_to_user_a_ctx().with_signer(&[&pool_seeds]),
            amount_a,
        )?;
        token::transfer(
            ctx.accounts.transfer_to_user_b_ctx().with_signer(&[&pool_seeds]),
            amount_b,
        )?;

        emit!(WithdrawEvent {
            user: ctx.accounts.user.key(),
            pool: ctx.accounts.pool.key(),
            lp_burned: lp_amount,
            amount_a_out: amount_a,
            amount_b_out: amount_b,
        });

        Ok(())
    }

    pub fn swap(
        ctx: Context<Swap>,
        amount_in: u64,
        min_amount_out: u64,
        direction: SwapDirection,
    ) -> Result<()> {
        require!(!ctx.accounts.pool.paused, AmmError::PoolPaused);
        require!(amount_in > 0, AmmError::InvalidAmount);

        let (reserve_in, reserve_out) = match direction {
            SwapDirection::AtoB => (ctx.accounts.vault_a.amount, ctx.accounts.vault_b.amount),
            SwapDirection::BtoA => (ctx.accounts.vault_b.amount, ctx.accounts.vault_a.amount),
        };
        require!(reserve_in > 0 && reserve_out > 0, AmmError::InsufficientLiquidity);

        let fee_bps = ctx.accounts.pool.fee_bps;
        let protocol_fee_bps = ctx.accounts.pool.protocol_fee_bps;
        require!(protocol_fee_bps <= fee_bps, AmmError::InvalidFee);

        let protocol_fee = (amount_in as u128)
            .checked_mul(protocol_fee_bps as u128)
            .ok_or(AmmError::MathOverflow)?
            .checked_div(BPS_DENOMINATOR as u128)
            .ok_or(AmmError::MathOverflow)? as u64;
        let amount_in_to_pool = amount_in
            .checked_sub(protocol_fee)
            .ok_or(AmmError::MathOverflow)?;
        let lp_fee_bps = fee_bps
            .checked_sub(protocol_fee_bps)
            .ok_or(AmmError::MathOverflow)?;

        let amount_out = quote_swap_out(amount_in_to_pool, reserve_in, reserve_out, lp_fee_bps)?;
        require!(amount_out >= min_amount_out, AmmError::SlippageExceeded);
        require!(amount_out < reserve_out, AmmError::InsufficientLiquidity);

        match direction {
            SwapDirection::AtoB => {
                require!(
                    ctx.accounts.user_source.mint == ctx.accounts.mint_a.key()
                        && ctx.accounts.user_destination.mint == ctx.accounts.mint_b.key(),
                    AmmError::InvalidSwapMint
                );
                token::transfer(ctx.accounts.transfer_to_vault_in_ctx(), amount_in_to_pool)?;
                if protocol_fee > 0 {
                    token::transfer(
                        ctx.accounts.transfer_to_fee_vault_ctx(),
                        protocol_fee,
                    )?;
                }
                let pool_seeds = ctx.accounts.pool.signer_seeds();
                token::transfer(
                    ctx.accounts
                        .transfer_to_user_out_ctx()
                        .with_signer(&[&pool_seeds]),
                    amount_out,
                )?;
            }
            SwapDirection::BtoA => {
                require!(
                    ctx.accounts.user_source.mint == ctx.accounts.mint_b.key()
                        && ctx.accounts.user_destination.mint == ctx.accounts.mint_a.key(),
                    AmmError::InvalidSwapMint
                );
                token::transfer(ctx.accounts.transfer_to_vault_in_ctx(), amount_in_to_pool)?;
                if protocol_fee > 0 {
                    token::transfer(
                        ctx.accounts.transfer_to_fee_vault_ctx(),
                        protocol_fee,
                    )?;
                }
                let pool_seeds = ctx.accounts.pool.signer_seeds();
                token::transfer(
                    ctx.accounts
                        .transfer_to_user_out_ctx()
                        .with_signer(&[&pool_seeds]),
                    amount_out,
                )?;
            }
        }

        emit!(SwapEvent {
            user: ctx.accounts.user.key(),
            pool: ctx.accounts.pool.key(),
            amount_in,
            amount_out,
            direction,
            protocol_fee,
        });

        Ok(())
    }

    pub fn withdraw_protocol_fees(
        ctx: Context<WithdrawProtocolFees>,
        amount_a: u64,
        amount_b: u64,
    ) -> Result<()> {
        require!(amount_a > 0 || amount_b > 0, AmmError::InvalidAmount);

        if amount_a > 0 {
            require!(
                ctx.accounts.fee_vault_a.amount >= amount_a,
                AmmError::InsufficientLiquidity
            );
        }
        if amount_b > 0 {
            require!(
                ctx.accounts.fee_vault_b.amount >= amount_b,
                AmmError::InsufficientLiquidity
            );
        }

        let pool_seeds = ctx.accounts.pool.signer_seeds();
        if amount_a > 0 {
            token::transfer(
                ctx.accounts
                    .transfer_fee_to_admin_a_ctx()
                    .with_signer(&[&pool_seeds]),
                amount_a,
            )?;
        }
        if amount_b > 0 {
            token::transfer(
                ctx.accounts
                    .transfer_fee_to_admin_b_ctx()
                    .with_signer(&[&pool_seeds]),
                amount_b,
            )?;
        }

        emit!(ProtocolFeeWithdrawEvent {
            admin: ctx.accounts.admin.key(),
            pool: ctx.accounts.pool.key(),
            amount_a,
            amount_b,
        });

        Ok(())
    }

    pub fn set_pause(ctx: Context<SetPause>, paused: bool) -> Result<()> {
        ctx.accounts.pool.paused = paused;

        emit!(PauseEvent {
            admin: ctx.accounts.admin.key(),
            pool: ctx.accounts.pool.key(),
            paused,
        });

        Ok(())
    }

    pub fn set_admin(ctx: Context<SetAdmin>) -> Result<()> {
        let old_admin = ctx.accounts.pool.admin;
        ctx.accounts.pool.admin = ctx.accounts.new_admin.key();

        emit!(AdminUpdatedEvent {
            pool: ctx.accounts.pool.key(),
            old_admin,
            new_admin: ctx.accounts.new_admin.key(),
        });

        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    pub admin: Signer<'info>,

    #[account(
        init,
        payer = payer,
        space = Pool::LEN,
        seeds = [b"pool", mint_a.key().as_ref(), mint_b.key().as_ref()],
        bump
    )]
    pub pool: Account<'info, Pool>,

    pub mint_a: Account<'info, Mint>,
    pub mint_b: Account<'info, Mint>,

    #[account(
        init,
        payer = payer,
        token::mint = mint_a,
        token::authority = pool,
        seeds = [b"vault_a", pool.key().as_ref()],
        bump
    )]
    pub vault_a: Account<'info, TokenAccount>,

    #[account(
        init,
        payer = payer,
        token::mint = mint_b,
        token::authority = pool,
        seeds = [b"vault_b", pool.key().as_ref()],
        bump
    )]
    pub vault_b: Account<'info, TokenAccount>,

    #[account(
        init,
        payer = payer,
        mint::decimals = mint_a.decimals,
        mint::authority = pool,
        seeds = [b"lp_mint", pool.key().as_ref()],
        bump
    )]
    pub lp_mint: Account<'info, Mint>,

    #[account(
        init,
        payer = payer,
        token::mint = mint_a,
        token::authority = pool,
        seeds = [b"fee_vault_a", pool.key().as_ref()],
        bump
    )]
    pub fee_vault_a: Account<'info, TokenAccount>,

    #[account(
        init,
        payer = payer,
        token::mint = mint_b,
        token::authority = pool,
        seeds = [b"fee_vault_b", pool.key().as_ref()],
        bump
    )]
    pub fee_vault_b: Account<'info, TokenAccount>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct DepositLiquidity<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(
        mut,
        has_one = mint_a,
        has_one = mint_b,
        has_one = vault_a,
        has_one = vault_b,
        has_one = lp_mint
    )]
    pub pool: Account<'info, Pool>,

    pub mint_a: Account<'info, Mint>,
    pub mint_b: Account<'info, Mint>,

    #[account(
        mut,
        constraint = vault_a.key() == pool.vault_a,
        constraint = vault_a.mint == mint_a.key(),
        constraint = vault_a.owner == pool.key()
    )]
    pub vault_a: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = vault_b.key() == pool.vault_b,
        constraint = vault_b.mint == mint_b.key(),
        constraint = vault_b.owner == pool.key()
    )]
    pub vault_b: Account<'info, TokenAccount>,

    #[account(mut, constraint = lp_mint.key() == pool.lp_mint)]
    pub lp_mint: Account<'info, Mint>,

    #[account(
        mut,
        constraint = user_ata_a.owner == user.key(),
        constraint = user_ata_a.mint == mint_a.key()
    )]
    pub user_ata_a: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = user_ata_b.owner == user.key(),
        constraint = user_ata_b.mint == mint_b.key()
    )]
    pub user_ata_b: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = user_lp.owner == user.key(),
        constraint = user_lp.mint == lp_mint.key()
    )]
    pub user_lp: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct WithdrawLiquidity<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(
        mut,
        has_one = mint_a,
        has_one = mint_b,
        has_one = vault_a,
        has_one = vault_b,
        has_one = lp_mint
    )]
    pub pool: Account<'info, Pool>,

    pub mint_a: Account<'info, Mint>,
    pub mint_b: Account<'info, Mint>,

    #[account(
        mut,
        constraint = vault_a.key() == pool.vault_a,
        constraint = vault_a.mint == mint_a.key(),
        constraint = vault_a.owner == pool.key()
    )]
    pub vault_a: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = vault_b.key() == pool.vault_b,
        constraint = vault_b.mint == mint_b.key(),
        constraint = vault_b.owner == pool.key()
    )]
    pub vault_b: Account<'info, TokenAccount>,

    #[account(mut, constraint = lp_mint.key() == pool.lp_mint)]
    pub lp_mint: Account<'info, Mint>,

    #[account(
        mut,
        constraint = user_ata_a.owner == user.key(),
        constraint = user_ata_a.mint == mint_a.key()
    )]
    pub user_ata_a: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = user_ata_b.owner == user.key(),
        constraint = user_ata_b.mint == mint_b.key()
    )]
    pub user_ata_b: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = user_lp.owner == user.key(),
        constraint = user_lp.mint == lp_mint.key()
    )]
    pub user_lp: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Swap<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(
        mut,
        has_one = mint_a,
        has_one = mint_b,
        has_one = vault_a,
        has_one = vault_b,
        has_one = fee_vault_a,
        has_one = fee_vault_b
    )]
    pub pool: Account<'info, Pool>,

    pub mint_a: Account<'info, Mint>,
    pub mint_b: Account<'info, Mint>,

    #[account(
        mut,
        constraint = vault_a.key() == pool.vault_a,
        constraint = vault_a.mint == mint_a.key(),
        constraint = vault_a.owner == pool.key()
    )]
    pub vault_a: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = vault_b.key() == pool.vault_b,
        constraint = vault_b.mint == mint_b.key(),
        constraint = vault_b.owner == pool.key()
    )]
    pub vault_b: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = fee_vault_a.key() == pool.fee_vault_a,
        constraint = fee_vault_a.mint == mint_a.key(),
        constraint = fee_vault_a.owner == pool.key()
    )]
    pub fee_vault_a: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = fee_vault_b.key() == pool.fee_vault_b,
        constraint = fee_vault_b.mint == mint_b.key(),
        constraint = fee_vault_b.owner == pool.key()
    )]
    pub fee_vault_b: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = user_source.owner == user.key()
    )]
    pub user_source: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = user_destination.owner == user.key()
    )]
    pub user_destination: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct WithdrawProtocolFees<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        mut,
        has_one = mint_a,
        has_one = mint_b,
        has_one = fee_vault_a,
        has_one = fee_vault_b,
        constraint = pool.admin == admin.key()
    )]
    pub pool: Account<'info, Pool>,

    pub mint_a: Account<'info, Mint>,
    pub mint_b: Account<'info, Mint>,

    #[account(
        mut,
        constraint = fee_vault_a.key() == pool.fee_vault_a,
        constraint = fee_vault_a.mint == mint_a.key(),
        constraint = fee_vault_a.owner == pool.key()
    )]
    pub fee_vault_a: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = fee_vault_b.key() == pool.fee_vault_b,
        constraint = fee_vault_b.mint == mint_b.key(),
        constraint = fee_vault_b.owner == pool.key()
    )]
    pub fee_vault_b: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = admin_ata_a.owner == admin.key(),
        constraint = admin_ata_a.mint == mint_a.key()
    )]
    pub admin_ata_a: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = admin_ata_b.owner == admin.key(),
        constraint = admin_ata_b.mint == mint_b.key()
    )]
    pub admin_ata_b: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct SetPause<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(mut, constraint = pool.admin == admin.key())]
    pub pool: Account<'info, Pool>,
}

#[derive(Accounts)]
pub struct SetAdmin<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    pub new_admin: Signer<'info>,

    #[account(mut, constraint = pool.admin == admin.key())]
    pub pool: Account<'info, Pool>,
}

#[account]
pub struct Pool {
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub vault_a: Pubkey,
    pub vault_b: Pubkey,
    pub lp_mint: Pubkey,
    pub fee_vault_a: Pubkey,
    pub fee_vault_b: Pubkey,
    pub admin: Pubkey,
    pub bump: u8,
    pub fee_bps: u16,
    pub protocol_fee_bps: u16,
    pub paused: bool,
}

impl Pool {
    pub const LEN: usize = 8 + 32 * 8 + 1 + 2 + 2 + 1;

    pub fn signer_seeds(&self) -> [&[u8]; 4] {
        [
            b"pool",
            self.mint_a.as_ref(),
            self.mint_b.as_ref(),
            &[self.bump],
        ]
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum SwapDirection {
    AtoB,
    BtoA,
}

#[event]
pub struct InitializeEvent {
    pub pool: Pubkey,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub lp_mint: Pubkey,
    pub vault_a: Pubkey,
    pub vault_b: Pubkey,
    pub fee_vault_a: Pubkey,
    pub fee_vault_b: Pubkey,
    pub fee_bps: u16,
    pub protocol_fee_bps: u16,
    pub admin: Pubkey,
    pub paused: bool,
}

#[event]
pub struct DepositEvent {
    pub user: Pubkey,
    pub pool: Pubkey,
    pub amount_a_in: u64,
    pub amount_b_in: u64,
    pub lp_minted: u64,
}

#[event]
pub struct WithdrawEvent {
    pub user: Pubkey,
    pub pool: Pubkey,
    pub lp_burned: u64,
    pub amount_a_out: u64,
    pub amount_b_out: u64,
}

#[event]
pub struct SwapEvent {
    pub user: Pubkey,
    pub pool: Pubkey,
    pub amount_in: u64,
    pub amount_out: u64,
    pub direction: SwapDirection,
    pub protocol_fee: u64,
}

#[event]
pub struct ProtocolFeeWithdrawEvent {
    pub admin: Pubkey,
    pub pool: Pubkey,
    pub amount_a: u64,
    pub amount_b: u64,
}

#[event]
pub struct PauseEvent {
    pub admin: Pubkey,
    pub pool: Pubkey,
    pub paused: bool,
}

#[event]
pub struct AdminUpdatedEvent {
    pub pool: Pubkey,
    pub old_admin: Pubkey,
    pub new_admin: Pubkey,
}

impl<'info> DepositLiquidity<'info> {
    fn transfer_to_vault_a_ctx(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            Transfer {
                from: self.user_ata_a.to_account_info(),
                to: self.vault_a.to_account_info(),
                authority: self.user.to_account_info(),
            },
        )
    }

    fn transfer_to_vault_b_ctx(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            Transfer {
                from: self.user_ata_b.to_account_info(),
                to: self.vault_b.to_account_info(),
                authority: self.user.to_account_info(),
            },
        )
    }

    fn mint_lp_ctx(&self) -> CpiContext<'_, '_, '_, 'info, MintTo<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            MintTo {
                mint: self.lp_mint.to_account_info(),
                to: self.user_lp.to_account_info(),
                authority: self.pool.to_account_info(),
            },
        )
    }
}

impl<'info> WithdrawLiquidity<'info> {
    fn burn_lp_ctx(&self) -> CpiContext<'_, '_, '_, 'info, Burn<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            Burn {
                mint: self.lp_mint.to_account_info(),
                from: self.user_lp.to_account_info(),
                authority: self.user.to_account_info(),
            },
        )
    }

    fn transfer_to_user_a_ctx(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            Transfer {
                from: self.vault_a.to_account_info(),
                to: self.user_ata_a.to_account_info(),
                authority: self.pool.to_account_info(),
            },
        )
    }

    fn transfer_to_user_b_ctx(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            Transfer {
                from: self.vault_b.to_account_info(),
                to: self.user_ata_b.to_account_info(),
                authority: self.pool.to_account_info(),
            },
        )
    }
}

impl<'info> Swap<'info> {
    fn transfer_to_vault_in_ctx(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let (from, to) = if self.user_source.mint == self.mint_a.key() {
            (
                self.user_source.to_account_info(),
                self.vault_a.to_account_info(),
            )
        } else {
            (
                self.user_source.to_account_info(),
                self.vault_b.to_account_info(),
            )
        };
        CpiContext::new(
            self.token_program.to_account_info(),
            Transfer {
                from,
                to,
                authority: self.user.to_account_info(),
            },
        )
    }

    fn transfer_to_user_out_ctx(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let (from, to) = if self.user_destination.mint == self.mint_b.key() {
            (
                self.vault_b.to_account_info(),
                self.user_destination.to_account_info(),
            )
        } else {
            (
                self.vault_a.to_account_info(),
                self.user_destination.to_account_info(),
            )
        };
        CpiContext::new(
            self.token_program.to_account_info(),
            Transfer {
                from,
                to,
                authority: self.pool.to_account_info(),
            },
        )
    }

    fn transfer_to_fee_vault_ctx(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let to = if self.user_source.mint == self.mint_a.key() {
            self.fee_vault_a.to_account_info()
        } else {
            self.fee_vault_b.to_account_info()
        };
        CpiContext::new(
            self.token_program.to_account_info(),
            Transfer {
                from: self.user_source.to_account_info(),
                to,
                authority: self.user.to_account_info(),
            },
        )
    }
}

impl<'info> WithdrawProtocolFees<'info> {
    fn transfer_fee_to_admin_a_ctx(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            Transfer {
                from: self.fee_vault_a.to_account_info(),
                to: self.admin_ata_a.to_account_info(),
                authority: self.pool.to_account_info(),
            },
        )
    }

    fn transfer_fee_to_admin_b_ctx(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            Transfer {
                from: self.fee_vault_b.to_account_info(),
                to: self.admin_ata_b.to_account_info(),
                authority: self.pool.to_account_info(),
            },
        )
    }
}

fn quote_swap_out(
    amount_in: u64,
    reserve_in: u64,
    reserve_out: u64,
    fee_bps: u16,
) -> Result<u64> {
    let amount_in_with_fee = (amount_in as u128)
        .checked_mul((BPS_DENOMINATOR - fee_bps as u64) as u128)
        .ok_or(AmmError::MathOverflow)?
        .checked_div(BPS_DENOMINATOR as u128)
        .ok_or(AmmError::MathOverflow)?;

    let numerator = amount_in_with_fee
        .checked_mul(reserve_out as u128)
        .ok_or(AmmError::MathOverflow)?;
    let denominator = (reserve_in as u128)
        .checked_add(amount_in_with_fee)
        .ok_or(AmmError::MathOverflow)?;

    numerator
        .checked_div(denominator)
        .ok_or(AmmError::MathOverflow)
        .map(|v| v as u64)
}

fn integer_sqrt(value: u128) -> u64 {
    if value == 0 {
        return 0;
    }
    let mut z = value;
    let mut x = value / 2 + 1;
    while x < z {
        z = x;
        x = (value / x + x) / 2;
    }
    z as u64
}

#[error_code]
pub enum AmmError {
    #[msg("Invalid amount")]
    InvalidAmount,
    #[msg("Slippage limit exceeded")]
    SlippageExceeded,
    #[msg("Non-proportional deposit")]
    NonProportionalDeposit,
    #[msg("Math overflow")]
    MathOverflow,
    #[msg("Insufficient liquidity")]
    InsufficientLiquidity,
    #[msg("Mints must be different")]
    SameMint,
    #[msg("Invalid swap mint accounts")]
    InvalidSwapMint,
    #[msg("Missing bump")]
    MissingBump,
    #[msg("Invalid fee configuration")]
    InvalidFee,
    #[msg("Pool is paused")]
    PoolPaused,
}
