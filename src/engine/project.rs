//! Projektdateien: Auswahl, Schnitte, Kapitelmarken und Kompression einer DVD sichern
//! und später wieder laden. Gespeichert wird nur, was der Nutzer eingestellt hat, und
//! der Ort der Quelle; die DVD selbst wird beim Laden neu eingelesen.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::dvd::model::Disc;
use crate::dvd::Res;
use crate::engine::plan::{ItemId, ItemSettings, Mode, Plan, ReauthorEntry, VideoMode};

/// Dateiendung der Projektdateien.
pub const EXTENSION: &str = "dvdproj";
const FORMAT: &str = "dvdmanager-projekt";
const VERSION: u32 = 1;

/// Merkmale einer DVD, an denen sich erkennen lässt, ob ein Projekt zu ihr gehört.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// Je Titel: Länge in Millisekunden und Anzahl der Kapitel
    pub titles: Vec<(u64, usize)>,
}

impl Fingerprint {
    pub fn of(disc: &Disc) -> Self {
        Self { titles: disc.titles.iter().map(|t| (t.ms, t.chapters.len())).collect() }
    }

    /// Gleiche Titel mit gleichen Kapiteln; Längen dürfen um eine Sekunde abweichen.
    pub fn matches(&self, other: &Fingerprint) -> bool {
        self.titles.len() == other.titles.len()
            && self.titles.iter().zip(&other.titles).all(|(a, b)| a.1 == b.1 && a.0.abs_diff(b.0) <= 1_000)
    }
}

/// Die Einstellungen eines Plans in speicherbarer Form.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanData {
    pub mode: Mode,
    pub target_sectors: u64,
    pub items: Vec<(ItemId, ItemSettings)>,
    pub reauthor: Vec<ReauthorEntry>,
    pub remove_puo: bool,
    pub region_free: bool,
    pub split_vob: bool,
    pub default_video: VideoMode,
}

impl From<&Plan> for PlanData {
    fn from(p: &Plan) -> Self {
        let mut items: Vec<(ItemId, ItemSettings)> = p.items.iter().map(|(k, v)| (*k, v.clone())).collect();
        items.sort_by_key(|(k, _)| match k {
            ItemId::Title(i) => (0, *i),
            ItemId::Menu(v) => (1, usize::from(*v)),
            ItemId::Unused(v) => (2, usize::from(*v)),
        });
        Self {
            mode: p.mode,
            target_sectors: p.target_sectors,
            items,
            reauthor: p.reauthor.clone(),
            remove_puo: p.remove_puo,
            region_free: p.region_free,
            split_vob: p.split_vob,
            default_video: p.default_video,
        }
    }
}

impl PlanData {
    pub fn to_plan(&self) -> Plan {
        let mut plan = Plan::new(self.target_sectors);
        plan.mode = self.mode;
        plan.items = self.items.iter().cloned().collect();
        plan.reauthor = self.reauthor.clone();
        plan.remove_puo = self.remove_puo;
        plan.region_free = self.region_free;
        plan.split_vob = self.split_vob;
        plan.default_video = self.default_video;
        plan
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub format: String,
    pub version: u32,
    /// Ordner, ISO-Datei oder Laufwerk, aus dem die DVD gelesen wurde
    pub source: PathBuf,
    pub label: String,
    pub fingerprint: Fingerprint,
    pub plan: PlanData,
}

impl Project {
    pub fn new(disc: &Disc, plan: &Plan) -> Self {
        Self {
            format: FORMAT.into(),
            version: VERSION,
            source: disc.source.origin.clone(),
            label: disc.source.label.clone(),
            fingerprint: Fingerprint::of(disc),
            plan: PlanData::from(plan),
        }
    }

    pub fn save(&self, path: &Path) -> Res<()> {
        let text = serde_json::to_string_pretty(self).map_err(|e| trf!("Projekt nicht speicherbar: {e}", "Project cannot be saved: {e}"))?;
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn load(path: &Path) -> Res<Project> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let p: Project = serde_json::from_str(&text)
            .map_err(|e| trf!("{} ist keine gültige Projektdatei ({e}).", "{} is not a valid project file ({e}).", path.display()))?;
        if p.format != FORMAT {
            return Err(trf!("{} ist keine Projektdatei des DVDmanagers.", "{} is not a DVDmanager project file.", path.display()));
        }
        if p.version > VERSION {
            return Err(tr!("Die Projektdatei stammt von einer neueren Programmversion.", "The project file comes from a newer program version.").into());
        }
        Ok(p)
    }

    /// Ist das eine Projektdatei (nach der Endung)?
    pub fn is_project_path(path: &Path) -> bool {
        path.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
    }

    /// Der gespeicherte Plan, geprüft gegen die tatsächlich geöffnete DVD.
    pub fn plan_for(&self, disc: &Disc) -> Res<Plan> {
        if !self.fingerprint.matches(&Fingerprint::of(disc)) {
            return Err(trf!(
                "Das Projekt gehört zu einer anderen DVD („{}“). Titel oder Kapitel stimmen nicht überein.", "The project belongs to a different DVD (\"{}\"). Titles or chapters do not match.",
                self.label
            ));
        }
        let plan = self.plan.to_plan();
        validate(&plan, disc)?;
        Ok(plan)
    }
}

/// Prüft, ob alle Verweise des Plans auf die DVD gültig sind.
pub fn validate(plan: &Plan, disc: &Disc) -> Res<()> {
    for id in plan.items.keys() {
        if let ItemId::Title(i) = id {
            if *i >= disc.titles.len() {
                return Err(trf!("Das Projekt nennt Titel {}, den es auf der DVD nicht gibt.", "The project refers to title {}, which does not exist on the DVD.", i + 1));
            }
        }
    }
    for (n, e) in plan.reauthor.iter().enumerate() {
        let Some(t) = disc.titles.get(e.title) else {
            return Err(trf!("Eintrag {} verweist auf einen fehlenden Titel.", "Entry {} refers to a missing title.", n + 1));
        };
        if e.first_chapter > e.last_chapter || e.last_chapter >= t.chapters.len() {
            return Err(trf!("Eintrag {} verweist auf fehlende Kapitel.", "Entry {} refers to missing chapters.", n + 1));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::plan::Cut;

    #[test]
    fn plan_round_trip() {
        let mut plan = Plan::new(12345);
        plan.mode = Mode::Reauthor;
        plan.default_video = VideoMode::Auto;
        plan.items.insert(ItemId::Title(1), ItemSettings { video: VideoMode::Custom(0.6), ..Default::default() });
        plan.items.insert(ItemId::Menu(0), ItemSettings { video: VideoMode::Still, ..Default::default() });
        let mut e = ReauthorEntry::new(1, 0, 2, ItemSettings::default());
        e.cut = Cut { start_ms: Some(1000), end_ms: Some(9000), exact: true };
        e.chapter_marks = Some(vec![3000, 6000]);
        e.settings.audio[2] = false;
        plan.reauthor.push(e);

        let text = serde_json::to_string(&PlanData::from(&plan)).unwrap();
        let back: PlanData = serde_json::from_str(&text).unwrap();
        let p2 = back.to_plan();
        assert_eq!(p2.mode, Mode::Reauthor);
        assert_eq!(p2.target_sectors, 12345);
        assert_eq!(p2.default_video, VideoMode::Auto);
        assert_eq!(p2.items.len(), 2);
        assert_eq!(p2.items[&ItemId::Title(1)].video, VideoMode::Custom(0.6));
        assert_eq!(p2.reauthor[0].cut, plan.reauthor[0].cut);
        assert_eq!(p2.reauthor[0].chapter_marks, Some(vec![3000, 6000]));
        assert!(!p2.reauthor[0].settings.audio[2]);
    }

    #[test]
    fn old_entries_without_marks_load() {
        let json = r#"{"title":0,"first_chapter":0,"last_chapter":1,
            "settings":{"video":"Keep","smoothing":"Off","audio":[true,true,true,true,true,true,true,true],
            "subs":[true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,
                    true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true]},
            "cut":{"start_ms":null,"end_ms":null,"exact":false}}"#;
        let e: ReauthorEntry = serde_json::from_str(json).unwrap();
        assert!(e.chapter_marks.is_none());
    }

    #[test]
    fn marks_are_normalized() {
        let mut m = vec![5000, 200, 5300, 12000, 7000, 7000];
        ReauthorEntry::normalize_marks(&mut m);
        assert_eq!(m, vec![5000, 7000, 12000]);
    }
}
