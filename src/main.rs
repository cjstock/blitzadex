mod cdragon;

use cdragon::{CDragon, PluginName};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut cdrag = CDragon::new()?;
    let status = cdrag.status(PluginName::RcpBeLolGameData).await?;
    dbg!(status);

    let _ = cdrag.update().await?;
    let status = cdrag.status(PluginName::RcpBeLolGameData).await?;
    dbg!(status);
    Ok(())
}
