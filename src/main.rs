mod cdragon;

use cdragon::{CDragon, PluginName};
use color_eyre::eyre::Result;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    Ok(())
}
