import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  createMint,
  getAccount,
  getOrCreateAssociatedTokenAccount,
  mintTo,
} from "@solana/spl-token";
import { assert } from "chai";

describe("amm", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.Amm as Program;

  it("accrues and withdraws protocol fees", async () => {
    const connection = provider.connection;
    const payer = (provider.wallet as any).payer as anchor.web3.Keypair;

    const decimals = 6;
    const mintA = await createMint(
      connection,
      payer,
      payer.publicKey,
      null,
      decimals
    );
    const mintB = await createMint(
      connection,
      payer,
      payer.publicKey,
      null,
      decimals
    );

    const [poolPda] = anchor.web3.PublicKey.findProgramAddressSync(
      [Buffer.from("pool"), mintA.toBuffer(), mintB.toBuffer()],
      program.programId
    );
    const [vaultA] = anchor.web3.PublicKey.findProgramAddressSync(
      [Buffer.from("vault_a"), poolPda.toBuffer()],
      program.programId
    );
    const [vaultB] = anchor.web3.PublicKey.findProgramAddressSync(
      [Buffer.from("vault_b"), poolPda.toBuffer()],
      program.programId
    );
    const [lpMint] = anchor.web3.PublicKey.findProgramAddressSync(
      [Buffer.from("lp_mint"), poolPda.toBuffer()],
      program.programId
    );
    const [feeVaultA] = anchor.web3.PublicKey.findProgramAddressSync(
      [Buffer.from("fee_vault_a"), poolPda.toBuffer()],
      program.programId
    );
    const [feeVaultB] = anchor.web3.PublicKey.findProgramAddressSync(
      [Buffer.from("fee_vault_b"), poolPda.toBuffer()],
      program.programId
    );

    await program.methods
      .initialize()
      .accounts({
        payer: payer.publicKey,
        admin: payer.publicKey,
        pool: poolPda,
        mintA,
        mintB,
        vaultA,
        vaultB,
        lpMint,
        feeVaultA,
        feeVaultB,
        systemProgram: anchor.web3.SystemProgram.programId,
        tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
      })
      .rpc();

    const userAtaA = await getOrCreateAssociatedTokenAccount(
      connection,
      payer,
      mintA,
      payer.publicKey
    );
    const userAtaB = await getOrCreateAssociatedTokenAccount(
      connection,
      payer,
      mintB,
      payer.publicKey
    );
    const userLp = await getOrCreateAssociatedTokenAccount(
      connection,
      payer,
      lpMint,
      payer.publicKey
    );

    await mintTo(
      connection,
      payer,
      mintA,
      userAtaA.address,
      payer,
      2_000_000_000
    );
    await mintTo(
      connection,
      payer,
      mintB,
      userAtaB.address,
      payer,
      4_000_000_000
    );

    const depositA = new anchor.BN(1_000_000);
    const depositB = new anchor.BN(2_000_000);
    await program.methods
      .depositLiquidity(depositA, depositB, new anchor.BN(0))
      .accounts({
        user: payer.publicKey,
        pool: poolPda,
        mintA,
        mintB,
        vaultA,
        vaultB,
        lpMint,
        userAtaA: userAtaA.address,
        userAtaB: userAtaB.address,
        userLp: userLp.address,
        tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
      })
      .rpc();

    const lpAfter = await getAccount(connection, userLp.address);
    assert(lpAfter.amount > 0n, "LP tokens should be minted");

    const reserveA = new anchor.BN(
      (await getAccount(connection, vaultA)).amount.toString()
    );
    const reserveB = new anchor.BN(
      (await getAccount(connection, vaultB)).amount.toString()
    );

    const amountIn = new anchor.BN(100_000);
    const feeBps = 30;
    const bpsDen = 10_000;
    const protocolFeeBps = 5;
    const amountInWithFee = amountIn.muln(bpsDen - feeBps).divn(bpsDen);
    const amountOut = amountInWithFee
      .mul(reserveB)
      .div(reserveA.add(amountInWithFee));
    const protocolFee = amountIn.muln(protocolFeeBps).divn(bpsDen);

    const destBefore = await getAccount(connection, userAtaB.address);
    const feeVaultBefore = await getAccount(connection, feeVaultA);

    await program.methods
      .swap(amountIn, amountOut, { aToB: {} })
      .accounts({
        user: payer.publicKey,
        pool: poolPda,
        mintA,
        mintB,
        vaultA,
        vaultB,
        feeVaultA,
        feeVaultB,
        userSource: userAtaA.address,
        userDestination: userAtaB.address,
        tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
      })
      .rpc();

    const destAfter = await getAccount(connection, userAtaB.address);
    const feeVaultAfter = await getAccount(connection, feeVaultA);

    const destDelta = new anchor.BN(
      (destAfter.amount - destBefore.amount).toString()
    );
    const feeDelta = new anchor.BN(
      (feeVaultAfter.amount - feeVaultBefore.amount).toString()
    );

    assert(
      destDelta.eq(amountOut),
      "User should receive expected output amount"
    );
    assert(
      feeDelta.eq(protocolFee),
      "Protocol fee vault should accrue expected fee"
    );

    const adminBefore = await getAccount(connection, userAtaA.address);

    await program.methods
      .withdrawProtocolFees(protocolFee, new anchor.BN(0))
      .accounts({
        admin: payer.publicKey,
        pool: poolPda,
        mintA,
        mintB,
        feeVaultA,
        feeVaultB,
        adminAtaA: userAtaA.address,
        adminAtaB: userAtaB.address,
        tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
      })
      .rpc();

    const adminAfter = await getAccount(connection, userAtaA.address);
    const feeVaultFinal = await getAccount(connection, feeVaultA);

    const adminDelta = new anchor.BN(
      (adminAfter.amount - adminBefore.amount).toString()
    );
    const feeDeltaAfter = new anchor.BN(
      (feeVaultAfter.amount - feeVaultFinal.amount).toString()
    );

    assert(
      adminDelta.eq(protocolFee),
      "Admin should receive withdrawn protocol fees"
    );
    assert(
      feeDeltaAfter.eq(protocolFee),
      "Fee vault should decrease by withdrawn amount"
    );
  });
});
