use std::{
    collections::HashMap,
    fs::{self, create_dir_all, File},
    io::BufReader,
    path::PathBuf,
    u64,
};

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use strum::Display;
use tokio::task::JoinHandle;

const GAME_DATA_URL: &str =
    "https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/v1";

#[derive(Debug, Default, Display)]
pub enum Status {
    #[default]
    Uninitialized,
    OutOfDate,
    UpToDate,
}

#[derive(Debug, Default)]
pub struct CDragon {
    http_client: reqwest::Client,
    cache_dir: PathBuf,
    data_dir: PathBuf,
    config_dir: PathBuf,
    status: Status,
    pub plugins: Vec<Plugin>,
    pub champions: HashMap<u64, Champion>,
}

impl CDragon {
    pub fn new() -> anyhow::Result<Self> {
        let proj_dirs = directories::ProjectDirs::from("", "", "blitzadex")
            .with_context(|| "failed to find your ")
            .unwrap();
        Ok(Self {
            status: Status::Uninitialized,
            http_client: reqwest::Client::new(),
            cache_dir: proj_dirs.cache_dir().into(),
            data_dir: proj_dirs.data_dir().into(),
            config_dir: proj_dirs.config_dir().into(),
            ..Default::default()
        })
    }

    async fn cached_plugin_updated_date(&self, name: &PluginName) -> Option<DateTime<Utc>> {
        let plugins: Result<Vec<Plugin>, anyhow::Error> = self.load_obj("plugins.json");
        plugins.map_or(None, |plugs| {
            plugs
                .iter()
                .find(|plug| plug.name == *name)
                .map_or(None, |p| Some(p.mtime))
        })
    }

    pub async fn status(&self, plugin_name: PluginName) -> anyhow::Result<Status> {
        let cached = self.cached_plugin_updated_date(&plugin_name).await;
        match cached {
            None => Ok(Status::OutOfDate),
            Some(cached_date) => {
                let fetched = self
                    .network_plugin_updated_date(&plugin_name)
                    .await
                    .map_err(|e| {
                        anyhow!("failed to check when {plugin_name} was last updated: {e}")
                    })?;
                if cached_date < fetched {
                    return Ok(Status::OutOfDate);
                } else {
                    return Ok(Status::UpToDate);
                }
            }
        }
    }

    /// Saves an object to $HOME/.cache/[`file_name`].
    ///
    /// When the $HOME/.cache/ directory doesn't exist, try to create it.
    ///
    /// # Args
    /// [`file_name`] - the name of this cache file ending with '.json'
    ///
    /// # Examples
    /// ```
    /// use cdragon::CDragon;
    ///
    /// let cdrag = CDragon::new().unwrap();
    /// let champions = cdrag.champions().await.unwrap();
    /// let _ = cdrag.save(&champions, "champions.json");
    /// ```
    fn save(&self, obj: &impl Serialize, file_name: impl Into<String>) -> anyhow::Result<()> {
        let ser = serde_json::to_string_pretty(obj)?;
        let mut file_path = self.cache_dir.clone();
        if file_path.try_exists().is_err()
            || file_path.try_exists().is_ok_and(|exists| exists == false)
        {
            create_dir_all(&file_path)?;
        }
        file_path.push(file_name.into());
        fs::write(file_path, ser)?;
        Ok(())
    }

    /// Loads a rust object from $HOME/.cache/[`file_name`].
    ///
    /// # Args
    /// [`file_name`] - the name of the cache file to load ending with '.json'
    ///
    /// # Examples
    /// ```
    /// use cdragon::CDragon;
    ///
    /// let cdrag = CDragon::new().unwrap();
    /// let champions = cdrag.load("champions.json").unwrap();
    /// ```
    pub fn load_obj<T>(&self, file_name: impl Into<String>) -> anyhow::Result<T>
    where
        for<'a> T: Deserialize<'a>,
    {
        let mut file_path = self.cache_dir.clone();
        file_path.push(file_name.into());
        let file = File::open(file_path)?;
        let reader = BufReader::new(file);
        let obj = serde_json::from_reader(reader)?;
        Ok(obj)
    }

    /// Fetches the latest CDragon data, and updates the [`CDragon.status`] to
    /// [`Status::UpToDate`]
    ///
    /// The fetched data is stored in fields of the [`CDragon`] struct. Currently
    /// only the [`Plugin`]s and [`Champion`]s are stored.
    ///
    ///
    pub async fn update(&mut self) -> anyhow::Result<()> {
        let plugins = self
            .plugins()
            .await
            .with_context(|| "failed to update plugins")?;
        self.save(&plugins, "plugins.json")
            .with_context(|| "failed to cache the updated plugins")?;
        self.plugins = plugins;

        let champions = self
            .all_champions()
            .await
            .with_context(|| "failed to update champions")?;
        self.save(&champions, "champion_details.json")
            .with_context(|| "failed to cache the updated champions")?;
        self.champions = champions;

        self.status = Status::UpToDate;
        Ok(())
    }

    /// Fetches the latest [`Plugin`]s from the CDragon API
    pub async fn plugins(&self) -> anyhow::Result<Vec<Plugin>> {
        let res = self
            .http_client
            .get(format!(
                "https://raw.communitydragon.org/json/latest/plugins/"
            ))
            .send()
            .await?
            .text()
            .await?;
        let plugins: Vec<Plugin> = serde_json::from_str(&res)?;
        Ok(plugins)
    }

    /// Checks when a specific [`Plugin`] was last updated via the CDragon API
    ///
    /// It is used in tandem with [CDragon::cached_plugin_updated_date] to calculate the status of
    /// the local CDragon instance.
    pub async fn network_plugin_updated_date(
        &self,
        name: &PluginName,
    ) -> anyhow::Result<DateTime<Utc>> {
        let plugins = self.plugins().await?;
        plugins
            .iter()
            .find_map(|plug| {
                if plug.name == *name {
                    Some(plug.mtime)
                } else {
                    None
                }
            })
            .ok_or(anyhow!("couldn't find the when {name:?} was last updated"))
    }

    pub async fn champion_ids(&self) -> anyhow::Result<Vec<u64>> {
        let res = self
            .http_client
            .get(format!("{GAME_DATA_URL}/champion-summary.json"))
            .send()
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

    pub async fn champion(&self, id: u64) -> anyhow::Result<Champion> {
        let res = self
            .http_client
            .get(format!("{GAME_DATA_URL}/champions/{id}.json"))
            .send()
            .await?
            .text()
            .await?;
        let champion = serde_json::from_str(&res)?;
        Ok(champion)
    }

    async fn champion_parallel(http_client: reqwest::Client, id: u64) -> anyhow::Result<Champion> {
        let res = http_client
            .get(format!("{GAME_DATA_URL}/champions/{id}.json"))
            .send()
            .await?
            .text()
            .await?;
        let champion = serde_json::from_str(&res)?;
        Ok(champion)
    }

    pub async fn all_champions(&self) -> anyhow::Result<HashMap<u64, Champion>> {
        let champ_ids = self.champion_ids().await?;
        let mut tasks: Vec<JoinHandle<_>> = Vec::with_capacity(champ_ids.len());
        for id in champ_ids {
            let client = self.http_client.clone();
            let task = tokio::spawn(Self::champion_parallel(client, id));
            tasks.push(task);
        }
        let mut champions = HashMap::with_capacity(tasks.len());
        for task in tasks {
            let champ = task.await??;
            champions.insert(champ.id.clone(), champ);
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

#[derive(Debug, Display, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PluginName {
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
        let res = CDragon::default().plugins().await;
        assert!(res.is_ok_and(|plugins| plugins
            .iter()
            .find(|plugin| plugin.name == PluginName::RcpBeLolGameData)
            .is_some()))
    }

    #[tokio::test]
    async fn get_champ_ids() {
        let res = CDragon::default().champion_ids().await;
        assert!(res.is_ok_and(|ids| ids.len() > 0))
    }

    #[tokio::test]
    async fn annie() {
        let res = CDragon::default().champion(1).await;
        assert!(res.is_ok_and(|annie| annie.name == "Annie" && annie.playstyle_info.damage == 3))
    }

    #[tokio::test]
    async fn champs_out_of_date() -> anyhow::Result<()> {
        let plugins = CDragon::default().plugins().await?;
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
        let champions = CDragon::default().all_champions().await?;
        assert!(champions.len() > 0);
        Ok(())
    }

    #[tokio::test]
    async fn update() -> anyhow::Result<()> {
        let mut cdrag = CDragon::new()?;
        cdrag.update().await?;
        Ok(())
    }
}
