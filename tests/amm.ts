import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  createMint,
  getAccount,
  getOrCreateAssociatedTokenAccount,
  getMint,
  mintTo,
} from "@solana/spl-token";
import { assert } from "chai";

describe("amm", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.Amm as Program;
  const feeBps = 30;
  const protocolFeeBps = 5;
  const bpsDen = 10_000;

  const toBn = (value: bigint) => new anchor.BN(value.toString());

  const airdrop = async (pubkey: anchor.web3.PublicKey) => {
    const signature = await provider.connection.requestAirdrop(
      pubkey,
      2 * anchor.web3.LAMPORTS_PER_SOL
    );
    await provider.connection.confirmTransaction(signature, "confirmed");
  };

  const setupPool = async () => {
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

    return {
      connection,
      payer,
      mintA,
      mintB,
      poolPda,
      vaultA,
      vaultB,
      lpMint,
      feeVaultA,
      feeVaultB,
      userAtaA,
      userAtaB,
      userLp,
    };
  };

  it("accrues and withdraws protocol fees", async () => {
    const {
      connection,
      payer,
      mintA,
      mintB,
      poolPda,
      vaultA,
      vaultB,
      lpMint,
      feeVaultA,
      feeVaultB,
      userAtaA,
      userAtaB,
      userLp,
    } = await setupPool();

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
    const protocolFee = amountIn.muln(protocolFeeBps).divn(bpsDen);
    const amountInToPool = amountIn.sub(protocolFee);
    const lpFeeBps = feeBps - protocolFeeBps;
    const amountInWithFee = amountInToPool.muln(bpsDen - lpFeeBps).divn(bpsDen);
    const amountOut = amountInWithFee
      .mul(reserveB)
      .div(reserveA.add(amountInWithFee));

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

  it("rejects non-admin protocol fee withdrawal", async () => {
    const {
      connection,
      payer,
      mintA,
      mintB,
      poolPda,
      vaultA,
      vaultB,
      lpMint,
      feeVaultA,
      feeVaultB,
      userAtaA,
      userAtaB,
      userLp,
    } = await setupPool();

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

    const reserveA = new anchor.BN(
      (await getAccount(connection, vaultA)).amount.toString()
    );
    const reserveB = new anchor.BN(
      (await getAccount(connection, vaultB)).amount.toString()
    );
    const amountIn = new anchor.BN(100_000);
    const protocolFee = amountIn.muln(protocolFeeBps).divn(bpsDen);
    const amountInToPool = amountIn.sub(protocolFee);
    const lpFeeBps = feeBps - protocolFeeBps;
    const amountInWithFee = amountInToPool.muln(bpsDen - lpFeeBps).divn(bpsDen);
    const amountOut = amountInWithFee
      .mul(reserveB)
      .div(reserveA.add(amountInWithFee));

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

    const attacker = anchor.web3.Keypair.generate();
    await airdrop(attacker.publicKey);
    const attackerAtaA = await getOrCreateAssociatedTokenAccount(
      connection,
      payer,
      mintA,
      attacker.publicKey
    );
    const attackerAtaB = await getOrCreateAssociatedTokenAccount(
      connection,
      payer,
      mintB,
      attacker.publicKey
    );

    let failed = false;
    try {
      await program.methods
        .withdrawProtocolFees(protocolFee, new anchor.BN(0))
        .accounts({
          admin: attacker.publicKey,
          pool: poolPda,
          mintA,
          mintB,
          feeVaultA,
          feeVaultB,
          adminAtaA: attackerAtaA.address,
          adminAtaB: attackerAtaB.address,
          tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
        })
        .signers([attacker])
        .rpc();
    } catch (err) {
      failed = true;
    }

    assert(failed, "Non-admin withdrawal should fail");
  });

  it("accepts imbalanced deposits", async () => {
    const {
      connection,
      payer,
      mintA,
      mintB,
      poolPda,
      vaultA,
      vaultB,
      lpMint,
      userAtaA,
      userAtaB,
      userLp,
    } = await setupPool();

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

    const reserveA = toBn((await getAccount(connection, vaultA)).amount);
    const reserveB = toBn((await getAccount(connection, vaultB)).amount);

    const imbalancedA = new anchor.BN(1_000_000);
    const imbalancedB = new anchor.BN(3_000_000);
    const idealB = imbalancedA.mul(reserveB).div(reserveA);

    const userBefore = await getAccount(connection, userAtaB.address);

    await program.methods
      .depositLiquidity(imbalancedA, imbalancedB, new anchor.BN(0))
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

    const userAfter = await getAccount(connection, userAtaB.address);
    const deltaB = toBn(userBefore.amount - userAfter.amount);
    assert(
      deltaB.eq(idealB),
      "Imbalanced deposit should only take ideal amount"
    );
  });

  it("withdraws liquidity proportionally", async () => {
    const {
      connection,
      payer,
      mintA,
      mintB,
      poolPda,
      vaultA,
      vaultB,
      lpMint,
      userAtaA,
      userAtaB,
      userLp,
    } = await setupPool();

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

    const lpMintInfo = await getMint(connection, lpMint);
    const totalLp = toBn(lpMintInfo.supply);
    const lpWithdraw = totalLp.divn(2);

    const reserveA = toBn((await getAccount(connection, vaultA)).amount);
    const reserveB = toBn((await getAccount(connection, vaultB)).amount);
    const expectedA = lpWithdraw.mul(reserveA).div(totalLp);
    const expectedB = lpWithdraw.mul(reserveB).div(totalLp);

    const beforeA = await getAccount(connection, userAtaA.address);
    const beforeB = await getAccount(connection, userAtaB.address);

    await program.methods
      .withdrawLiquidity(lpWithdraw, new anchor.BN(0), new anchor.BN(0))
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

    const afterA = await getAccount(connection, userAtaA.address);
    const afterB = await getAccount(connection, userAtaB.address);
    const deltaA = toBn(afterA.amount - beforeA.amount);
    const deltaB = toBn(afterB.amount - beforeB.amount);

    assert(deltaA.eq(expectedA), "Withdrawn amount A should be proportional");
    assert(deltaB.eq(expectedB), "Withdrawn amount B should be proportional");
  });
});
