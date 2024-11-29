use std::{collections::HashMap, u64};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use strum::Display;
use tokio::task::JoinHandle;

const GAME_DATA_URL: &str =
    "https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/v1";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CDragon {
    pub plugins: Vec<Plugin>,
    pub champion_ids: Vec<u64>,
    pub champions: HashMap<String, Champion>,
}

impl CDragon {
    pub async fn plugins() -> anyhow::Result<Vec<Plugin>> {
        let res = reqwest::get(format!(
            "https://raw.communitydragon.org/json/latest/plugins/"
        ))
        .await?
        .text()
        .await?;
        let plugins: Vec<Plugin> = serde_json::from_str(&res)?;
        Ok(plugins)
    }

    pub async fn champion_ids() -> anyhow::Result<Vec<u64>> {
        let res = reqwest::get(format!("{GAME_DATA_URL}/champion-summary.json"))
            .await?
            .text()
            .await?;
        let obj: Vec<Value> = serde_json::from_str(&res)?;
        let champ_ids: Vec<u64> = obj
            .iter()
            .skip(1)
            .map(|v| v.get("id").unwrap().as_u64().unwrap())
            .collect();
        Ok(champ_ids)
    }

    pub async fn champion(id: u64) -> anyhow::Result<Champion> {
        let res = reqwest::get(format!("{GAME_DATA_URL}/champions/{id}.json"))
            .await?
            .text()
            .await?;
        let champion = serde_json::from_str(&res)?;
        Ok(champion)
    }

    pub async fn all_champions() -> anyhow::Result<HashMap<String, Champion>> {
        let champ_ids = Self::champion_ids().await?;
        let mut tasks: Vec<JoinHandle<_>> = Vec::with_capacity(champ_ids.len());
        for id in champ_ids {
            let task = tokio::spawn(Self::champion(id));
            tasks.push(task);
        }
        let mut champions = HashMap::with_capacity(tasks.len());
        for task in tasks {
            let champ = task.await??;
            champions.insert(champ.name.clone(), champ);
        }
        Ok(champions)
    }
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TactialInfo {
    style: u64,
    difficulty: u64,
    damage_type: String,
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PlaystyleInfo {
    damage: u64,
    durability: u64,
    crowd_control: u64,
    mobility: u64,
    utility: u64,
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Champion {
    id: u64,
    name: String,
    alias: String,
    title: String,
    short_bio: String,
    tactical_info: TactialInfo,
    playstyle_info: PlaystyleInfo,
    square_portrait_path: String,
    stinger_sfx_path: String,
    choose_vo_path: String,
    ban_vo_path: String,
    roles: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum PluginName {
    #[default]
    None,
    RcpBeLolGameData,
    RcpBeLolLicenseAgreement,
    RcpBeSanitizer,
    RcpFeAudio,
    RcpFeCommonLibs,
    RcpFeEmberLibs,
    RcpFeLolCareerStats,
    RcpFeLolChampSelect,
    RcpFeLolChampionDetails,
    RcpFeLolChampionStatistics,
    RcpFeLolClash,
    RcpFeLolCollections,
    RcpFeLolEsportsSpectate,
    RcpFeLolEventHub,
    RcpFeLolEventShop,
    RcpFeLolHighlights,
    RcpFeLolHonor,
    RcpFeLolKickout,
    RcpFeLolL10n,
    RcpFeLolLeagues,
    RcpFeLolLockAndLoad,
    RcpFeLolLoot,
    RcpFeLolMatchHistory,
    RcpFeLolNavigation,
    RcpFeLolNewPlayerExperience,
    RcpFeLolNpeRewards,
    RcpFeLolParties,
    RcpFeLolPaw,
    RcpFeLolPft,
    RcpFeLolPostgame,
    RcpFeLolPremadeVoice,
    RcpFeLolProfiles,
    RcpFeLolSettings,
    RcpFeLolSharedComponents,
    RcpFeLolSkinsPicker,
    RcpFeLolSocial,
    RcpFeLolStartup,
    RcpFeLolStaticAssets,
    RcpFeLolStore,
    RcpFeLolTft,
    RcpFeLolTftTeamPlanner,
    RcpFeLolTftTroves,
    RcpFeLolTypekit,
    RcpFeLolUikit,
    RcpFeLolYourshop,
    RcpFePluginRunner,
    #[serde(other)]
    PluginManifest,
}

#[derive(Display, Debug, Serialize, Deserialize)]
enum PluginType {
    #[serde(rename = "file")]
    File,
    #[serde(rename = "directory")]
    Directory,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Plugin {
    name: PluginName,
    #[serde(rename = "type")]
    ty: PluginType,
    #[serde(with = "mtime_format")]
    mtime: DateTime<Utc>,
    size: Option<i32>,
}

impl Plugin {
    pub fn updated_since(&self, date: DateTime<Utc>) -> bool {
        self.mtime > date
    }
}

mod mtime_format {
    use chrono::{DateTime, NaiveDateTime, Utc};
    use serde::{self, Deserialize, Deserializer, Serializer};

    const FORMAT: &'static str = "%a, %d %b %Y %H:%M:%S %Z";

    pub fn serialize<S>(date: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = format!("{}", date.format(FORMAT));
        serializer.serialize_str(&s)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let dt = NaiveDateTime::parse_from_str(&s, FORMAT).map_err(serde::de::Error::custom)?;
        Ok(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use chrono::{Datelike, Local};

    #[tokio::test]
    async fn get_plugs() {
        let res = CDragon::plugins().await;
        assert!(res.is_ok_and(|plugins| plugins
            .iter()
            .find(|plugin| plugin.name == PluginName::RcpBeLolGameData)
            .is_some()))
    }

    #[tokio::test]
    async fn get_champ_ids() {
        let res = CDragon::champion_ids().await;
        assert!(res.is_ok_and(|ids| ids.len() > 0))
    }

    #[tokio::test]
    async fn annie() {
        let res = CDragon::champion(1).await;
        assert!(res.is_ok_and(|annie| annie.name == "Annie" && annie.playstyle_info.damage == 3))
    }

    #[tokio::test]
    async fn champs_out_of_date() -> anyhow::Result<()> {
        let plugins = CDragon::plugins().await?;
        let champs_plugin = plugins
            .iter()
            .find(|plugin| plugin.name == PluginName::RcpBeLolGameData)
            .unwrap();
        let one_year_ago = Local::now().with_year(2023).unwrap();
        let updated_since_a_year_ago = champs_plugin.updated_since(one_year_ago.into());
        assert!(updated_since_a_year_ago);
        let one_year_from_now = Local::now().with_year(2025).unwrap();
        let updated_since_one_year_from_now = champs_plugin.updated_since(one_year_from_now.into());
        assert!(!updated_since_one_year_from_now);
        Ok(())
    }

    #[tokio::test]
    async fn all_champs() -> anyhow::Result<()> {
        let champions = CDragon::all_champions().await?;
        assert!(champions.len() > 0);
        Ok(())
    }
}
