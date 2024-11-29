mod cdragon;

use cdragon::CDragon;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _res = CDragon::all_champions().await?;
    Ok(())
}
