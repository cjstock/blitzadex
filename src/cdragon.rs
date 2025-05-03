use std::{
    collections::HashMap,
    fmt::Display,
    fs::{self, create_dir_all, File},
    io::{self, BufReader},
    path::PathBuf,
    u64,
};

use chrono::{DateTime, Utc};
use color_eyre::{
    eyre::{eyre, Context, ContextCompat},
    Result,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use strum::Display;
use tokio::task::JoinHandle;

const GAME_DATA_URL: &str =
    "https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default";
const V1: &str = "v1";
const ASSETS: &str = "assets";

#[derive(Debug, Default, Display)]
pub enum Status {
    #[default]
    Uninitialized,
    OutOfDate,
    UpToDate,
}

#[derive(Debug)]
enum CacheFile {
    Plugins,
    Champions,
}

impl Display for CacheFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Plugins => "plugins.json",
            Self::Champions => "champions.json",
        };
        f.write_str(s)
    }
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
    pub async fn new() -> color_eyre::Result<Self> {
        let proj_dirs = directories::ProjectDirs::from("", "", "blitzadex")
            .with_context(|| "failed to find the project directory")
            .unwrap();
        let cache_dir = proj_dirs.cache_dir().to_path_buf();
        let data_dir = proj_dirs.data_dir().to_path_buf();
        let config_dir = proj_dirs.config_dir().to_path_buf();
        let mut cdrag = Self {
            status: Status::Uninitialized,
            http_client: reqwest::Client::new(),
            cache_dir,
            data_dir,
            config_dir,
            ..Default::default()
        };
        cdrag.plugins = cdrag
            .load_obj(CacheFile::Plugins)
            .unwrap_or(cdrag.fetch_plugins().await?);
        cdrag.champions = cdrag
            .load_obj(CacheFile::Champions)
            .unwrap_or(cdrag.fetch_all_champions().await?);
        Ok(cdrag)
    }

    pub fn champion_by_name<'a, N: Into<String> + Copy>(&'a self, name: N) -> Option<&'a Champion> {
        self.champions
            .iter()
            .find(|champ| champ.1.name == name.into())
            .map(|champ| champ.1)
    }

    pub fn clean_up(&self) -> color_eyre::Result<()> {
        fs::remove_dir_all(&self.cache_dir).ok();
        fs::remove_dir_all(&self.data_dir).ok();
        fs::remove_dir_all(&self.config_dir).ok();
        Ok(())
    }

    async fn cached_plugin_updated_date(&self, name: &PluginName) -> Option<DateTime<Utc>> {
        let plugins: Result<Vec<Plugin>, color_eyre::eyre::Error> =
            self.load_obj(CacheFile::Plugins);
        plugins.map_or(None, |plugs| {
            plugs
                .iter()
                .find(|plug| plug.name == *name)
                .map_or(None, |p| Some(p.mtime))
        })
    }

    pub async fn status(&self, plugin_name: PluginName) -> color_eyre::Result<Status> {
        let cached = self.cached_plugin_updated_date(&plugin_name).await;
        match cached {
            None => Ok(Status::OutOfDate),
            Some(cached_date) => {
                let fetched = self
                    .network_plugin_updated_date(&plugin_name)
                    .await
                    .map_err(|e| {
                        eyre!("failed to check when {plugin_name} was last updated: {e}")
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
    fn cache_obj(&self, obj: &impl Serialize, cache_file: CacheFile) -> color_eyre::Result<()> {
        let ser = serde_json::to_string_pretty(obj)?;
        let mut file_path = self.cache_dir.clone();
        if file_path.try_exists().is_err()
            || file_path.try_exists().is_ok_and(|exists| exists == false)
        {
            create_dir_all(&file_path)?;
        }
        file_path.push(cache_file.to_string());
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
    /// let champions = cdrag.load(CacheFile::Champions).unwrap();
    /// ```
    fn load_obj<T>(&self, cache_file: CacheFile) -> color_eyre::Result<T>
    where
        for<'a> T: Deserialize<'a>,
    {
        let mut file_path = self.cache_dir.clone();
        file_path.push(cache_file.to_string());
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
    pub async fn update(&mut self) -> color_eyre::Result<()> {
        let plugins = self
            .fetch_plugins()
            .await
            .with_context(|| "failed to update plugins")?;
        self.cache_obj(&plugins, CacheFile::Plugins)
            .with_context(|| "failed to cache the updated plugins")?;
        self.plugins = plugins;

        let champions = self
            .fetch_all_champions()
            .await
            .with_context(|| "failed to update champions")?;
        self.cache_obj(&champions, CacheFile::Champions)
            .with_context(|| "failed to cache the updated champions")?;
        self.champions = champions;

        self.status = Status::UpToDate;
        Ok(())
    }

    /// Fetches the latest [`Plugin`]s from the CDragon API
    pub async fn fetch_plugins(&self) -> color_eyre::Result<Vec<Plugin>> {
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
    ) -> color_eyre::Result<DateTime<Utc>> {
        let plugins = self.fetch_plugins().await?;
        plugins
            .iter()
            .find_map(|plug| {
                if plug.name == *name {
                    Some(plug.mtime)
                } else {
                    None
                }
            })
            .ok_or(eyre!("couldn't find the when {name:?} was last updated"))
    }

    pub async fn fetch_champion_ids(&self) -> color_eyre::Result<Vec<u64>> {
        let res = self
            .http_client
            .get(format!("{GAME_DATA_URL}/{V1}/champion-summary.json"))
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

    pub async fn fetch_champion(&self, id: u64) -> color_eyre::Result<Champion> {
        let res = self
            .http_client
            .get(format!("{GAME_DATA_URL}/{V1}/champions/{id}.json"))
            .send()
            .await?
            .text()
            .await?;
        let champion = serde_json::from_str(&res)?;
        Ok(champion)
    }

    async fn fetch_champion_parallel(
        http_client: reqwest::Client,
        id: u64,
    ) -> color_eyre::Result<Champion> {
        let res = http_client
            .get(format!("{GAME_DATA_URL}/{V1}/champions/{id}.json"))
            .send()
            .await?
            .text()
            .await?;
        let champion = serde_json::from_str(&res)?;
        Ok(champion)
    }

    pub async fn fetch_all_champions(&self) -> color_eyre::Result<HashMap<u64, Champion>> {
        let champ_ids = self.fetch_champion_ids().await?;
        let mut tasks: Vec<JoinHandle<_>> = Vec::with_capacity(champ_ids.len());
        for id in champ_ids {
            let client = self.http_client.clone();
            let task = tokio::spawn(Self::fetch_champion_parallel(client, id));
            tasks.push(task);
        }
        let mut champions = HashMap::with_capacity(tasks.len());
        for task in tasks {
            let champ = task.await??;
            champions.insert(champ.id.clone(), champ);
        }
        Ok(champions)
    }

    pub async fn download_skin_asset(
        &self,
        skin: &Skin,
        asset: &SkinAsset,
    ) -> color_eyre::Result<()> {
        let asset_path = self.skin_path_of(skin, asset)?;
        let asset_url = format!("{GAME_DATA_URL}/{}", asset_path.to_str().unwrap());
        let bytes = self
            .http_client
            .get(asset_url)
            .send()
            .await
            .with_context(|| "couldn't download asset")?
            .bytes()
            .await?;
        let file_path = self.data_dir.join(asset_path);
        let mut file_dir = file_path.clone();
        file_dir.pop();
        create_dir_all(&file_dir)?;
        let mut file = File::create(file_path).with_context(|| "couldn't create skin file")?;
        io::copy(&mut bytes.as_ref(), &mut file).with_context(|| "couldn't copy bytes")?;
        Ok(())
    }

    pub fn skin_path_of(&self, skin: &Skin, asset: &SkinAsset) -> color_eyre::Result<PathBuf> {
        let asset_path = match asset {
            SkinAsset::Tile => &skin.tile_path,
            SkinAsset::Splash => &skin.splash_path,
            SkinAsset::LoadScreen => &skin.load_screen_path,
            SkinAsset::UncenteredSplash => &skin.uncentered_splash_path,
        };
        Ok(asset_path.into())
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
pub enum Rarity {
    KEpic,
    KLegendary,
    KMythic,
    #[default]
    KNoRarity,
    KRare,
    KTranscendent,
    KUltimate,
    KExalted,
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum SkinType {
    Ultimate,
    #[default]
    #[serde(other)]
    None,
}

/// The information and asset paths for a [`Skin`]
///
///
/// [`splash_path`] - [Normalized Path] to the splash art centered on the skin
/// [`uncentered_splash_path`] - [Normalized Path] to normal splash art for the skin. May overlap
/// with splash arts for skins in the same [`skin_lines`] because a single splash art includes all
/// of the champions.
/// tile_path
/// load_screen_path
///
/// ## [Normalized Path]
/// Paths returned by the Cdragon api's json cannot be used to directly navigate to an asset. To
/// [Normalize] this path we strip the first two path parts and cast to lowercase.
///
/// This normalization will allow us to construct the actual path to the asset by doing the following:
/// ```
/// let cdragon = CDragon::new().await.unwrap();
/// let akshan_skin_splash = cdragon.champions
/// format!("{GAME_DATA_URL}/{ASSETS}/")
/// ```
///
/// For example:
///     From the Cdragon json:
///     /lol-game-data/assets/**ASSETS**/Characters/Akshan/Skins/Base/Images/akshan_splash_uncentered_0.jpg
///
///     The path to the actual asset:
///     https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/assets/characters/akshan/skins/base/images/akshan_splash_uncentered_0.jpg
///
///     [Normalized Path]:
///     assets/characters/akshan/skins/base/images/akshan_splash_uncentered_0.jpg
///
///
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Skin {
    id: u64,
    is_base: bool,
    name: String,
    #[serde(deserialize_with = "deserialize_asset_path")]
    splash_path: String,
    #[serde(deserialize_with = "deserialize_asset_path")]
    uncentered_splash_path: String,
    #[serde(deserialize_with = "deserialize_asset_path")]
    tile_path: String,
    #[serde(deserialize_with = "deserialize_asset_path")]
    load_screen_path: String,
    skin_type: SkinType,
    rarity: Rarity,
    is_legacy: bool,
    #[serde(deserialize_with = "deserialize_skin_lines")]
    skin_lines: Vec<u64>,
    description: Option<String>,
}

pub enum SkinAsset {
    Splash,
    UncenteredSplash,
    Tile,
    LoadScreen,
}

fn deserialize_asset_path<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let path = String::deserialize(deserializer)?
        .replace("/lol-game-data/assets/ASSETS", ASSETS)
        .to_lowercase();
    Ok(path)
}

fn deserialize_skin_lines<'de, D>(deserializer: D) -> Result<Vec<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let mut res = vec![];
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.to_string() == "null" {
        return Ok(res);
    }
    // TODO: I don't love these errors, but I haven't quite figured out how to properly map them.
    for j_struct in value.as_array().unwrap() {
        let v = j_struct
            .as_object()
            .ok_or(serde::de::Error::custom("that's not an object"))?
            .get("id")
            .ok_or(serde::de::Error::missing_field("id"))?
            .as_u64()
            .ok_or(serde::de::Error::custom("that's not a u64"))?;
        res.push(v);
    }
    Ok(res)
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SkinLine {
    id: u64,
    name: String,
    description: String,
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
    roles: Vec<String>,
    skins: Vec<Skin>,
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
    use rstest::*;

    #[tokio::test]
    async fn get_plugs() {
        let res = CDragon::default().fetch_plugins().await;
        assert!(res.is_ok_and(|plugins| plugins
            .iter()
            .find(|plugin| plugin.name == PluginName::RcpBeLolGameData)
            .is_some()))
    }

    #[tokio::test]
    async fn get_champ_ids() {
        let res = CDragon::default().fetch_champion_ids().await;
        assert!(res.is_ok_and(|ids| ids.len() > 0))
    }

    #[tokio::test]
    async fn annie() {
        let res = CDragon::default().fetch_champion(1).await;
        assert!(res.is_ok_and(|annie| annie.name == "Annie" && annie.playstyle_info.damage == 3))
    }

    #[tokio::test]
    async fn champs_out_of_date() -> color_eyre::Result<()> {
        let plugins = CDragon::default().fetch_plugins().await?;
        let champs_plugin = plugins
            .iter()
            .find(|plugin| plugin.name == PluginName::RcpBeLolGameData)
            .unwrap();
        let one_year_ago = Local::now().with_year(2023).unwrap();
        let updated_since_a_year_ago = champs_plugin.updated_since(one_year_ago.into());
        assert!(updated_since_a_year_ago);
        let one_year_from_now = Local::now().with_year(2026).unwrap();
        let updated_since_one_year_from_now = champs_plugin.updated_since(one_year_from_now.into());
        assert!(!updated_since_one_year_from_now);
        Ok(())
    }

    #[fixture]
    async fn cdrag_instance() -> color_eyre::Result<CDragon> {
        CDragon::new().await
    }

    #[rstest]
    #[tokio::test]
    async fn all_champs(
        #[future] cdrag_instance: color_eyre::Result<CDragon>,
    ) -> color_eyre::Result<()> {
        let cdrag = cdrag_instance.await?;
        assert!(cdrag.champions.len() > 0);
        Ok(())
    }

    #[rstest]
    #[tokio::test]
    async fn update(
        #[future] cdrag_instance: color_eyre::Result<CDragon>,
    ) -> color_eyre::Result<()> {
        let mut cdrag = cdrag_instance.await?;
        cdrag.update().await?;
        Ok(())
    }

    #[rstest]
    #[tokio::test]
    async fn cleanup(
        #[future] cdrag_instance: color_eyre::Result<CDragon>,
    ) -> color_eyre::Result<()> {
        let cdrag = cdrag_instance.await?;
        cdrag.clean_up()?;

        let cache_exists = cdrag.cache_dir.try_exists().unwrap_or(false);
        assert!(!cache_exists);
        let config_exists = cdrag.config_dir.try_exists().unwrap_or(false);
        assert!(!config_exists);
        let data_exists = cdrag.data_dir.try_exists().unwrap_or(false);
        assert!(!data_exists);

        Ok(())
    }

    #[rstest]
    #[tokio::test]
    async fn get_akshan(
        #[future] cdrag_instance: color_eyre::Result<CDragon>,
    ) -> color_eyre::Result<()> {
        let cdrag = cdrag_instance.await?;
        let akshan = cdrag.champion_by_name("Akshan");
        assert!(akshan.is_some_and(|ak| ak.id == 166));
        Ok(())
    }

    #[rstest]
    #[tokio::test]
    async fn download_akshan_base_tile(
        #[future] cdrag_instance: color_eyre::Result<CDragon>,
    ) -> color_eyre::Result<()> {
        let cdrag = cdrag_instance.await?;
        let akshan = cdrag.champion_by_name("Akshan").unwrap();
        let base_skin = akshan.skins.iter().find(|skin| skin.is_base).unwrap();
        cdrag
            .download_skin_asset(base_skin, &SkinAsset::Tile)
            .await?;
        Ok(())
    }

    #[rstest]
    #[tokio::test]
    async fn download_akshan_base_uncentered(
        #[future] cdrag_instance: color_eyre::Result<CDragon>,
    ) -> color_eyre::Result<()> {
        let cdrag = cdrag_instance.await?;
        let akshan = cdrag.champion_by_name("Akshan").unwrap();
        let base_skin = akshan.skins.iter().find(|skin| skin.is_base).unwrap();
        cdrag
            .download_skin_asset(base_skin, &SkinAsset::UncenteredSplash)
            .await?;
        Ok(())
    }
}
