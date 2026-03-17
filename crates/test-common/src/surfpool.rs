use crate::{util::hex_encode, RpcContext};
use anyhow::Result;
use serde_json::json;
use solana_client::rpc_request::RpcRequest;
use solana_signer::Signer;

pub fn deploy_program_surfpool(ctx: &RpcContext, so_path: &str) -> Result<()> {
    let so_bytes = std::fs::read(so_path)
        .map_err(|e| anyhow::anyhow!("Failed to read program .so at {}: {}", so_path, e))?;

    let program_id = ctx.program_id.to_string();

    let chunk_size = 4 * 1024 * 1024;
    let total_chunks = (so_bytes.len() + chunk_size - 1) / chunk_size;

    println!(
        "Deploying program {} ({} bytes, {} chunks)...",
        program_id,
        so_bytes.len(),
        total_chunks,
    );

    for (i, chunk) in so_bytes.chunks(chunk_size).enumerate() {
        let offset = i * chunk_size;
        let hex_data = hex_encode(chunk);

        let resp: serde_json::Value = ctx.rpc_client.send::<serde_json::Value>(
            RpcRequest::Custom {
                method: "surfnet_writeProgram",
            },
            json!([program_id, hex_data, offset, ctx.payer.pubkey().to_string()]),
        )?;

        if let Some(err) = resp.get("error") {
            anyhow::bail!(
                "surfnet_writeProgram failed at chunk {}/{}: {}",
                i + 1,
                total_chunks,
                err,
            );
        }
    }

    println!("Program {} deployed successfully.", program_id);
    Ok(())
}
