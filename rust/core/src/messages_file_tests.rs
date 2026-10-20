use super::{Message, MessageCatalog};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const ENGLISH: &str = include_str!("../../../messages/en-US.ftl");

#[test]
fn composition_catalog_admin_english_is_additive_per_attribute() {
    let directories = TemporaryCatalogs::new();
    fs::write(
        directories.admin().join("en-US.ftl"),
        "no-links =\n    .text = ADMIN\n",
    )
    .unwrap();
    let catalog = MessageCatalog::load_from_catalog_dirs(
        "en-US",
        &[directories.admin()],
        &[directories.package()],
    )
    .unwrap();
    let forms = catalog.format(&Message::NoLinks).unwrap();
    assert_eq!(forms.text, "ADMIN");
    assert_eq!(forms.tts, "No links");
    assert_eq!(
        catalog.format(&Message::LastKeyedNone).unwrap().morse,
        "NO LAST KEYED"
    );
}

#[test]
fn composition_catalog_adds_custom_ids_without_copying_shipped_messages() {
    let directories = TemporaryCatalogs::new();
    fs::write(
        directories.admin().join("en-US.ftl"),
        "site-custom =\n    .text = Battery\n    .tts = Battery voltage\n    .morse = BATTERY\n",
    )
    .unwrap();
    fs::write(
        directories.admin().join("fr.ftl"),
        "site-custom =\n    .tts = Tension\n",
    )
    .unwrap();
    let catalog =
        MessageCatalog::load_from_catalog_dirs("fr", &[directories.admin()], &[]).unwrap();
    let custom = catalog
        .format_named("site-custom", &Default::default())
        .unwrap();
    assert_eq!(custom.tts, "Tension");
    assert_eq!(custom.morse, "BATTERY");
    assert!(catalog.format(&Message::NoLinks).is_ok());
    assert!(catalog.translation_warnings().is_empty());
}

#[test]
fn composition_catalog_admin_translation_keeps_package_attributes() {
    let directories = TemporaryCatalogs::new();
    fs::write(
        directories.admin().join("fr-CA.ftl"),
        "no-links =\n    .text = ADMIN\n    .tts = { $invalid }\n",
    )
    .unwrap();
    fs::write(
        directories.package().join("fr-CA.ftl"),
        "no-links =\n    .tts = PACKAGE\n",
    )
    .unwrap();
    let catalog = MessageCatalog::load_from_catalog_dirs(
        "fr-CA",
        &[directories.admin()],
        &[directories.package()],
    )
    .unwrap();
    assert_eq!(catalog.format(&Message::NoLinks).unwrap().tts, "PACKAGE");
}

struct TemporaryCatalogs(PathBuf);

impl TemporaryCatalogs {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("rptadv-messages-{unique}"));
        fs::create_dir_all(path.join("admin")).unwrap();
        fs::create_dir_all(path.join("package")).unwrap();
        fs::create_dir_all(path.join("standalone")).unwrap();
        Self(path)
    }

    fn admin(&self) -> PathBuf {
        self.0.join("admin")
    }

    fn package(&self) -> PathBuf {
        self.0.join("package")
    }

    fn standalone(&self) -> PathBuf {
        self.0.join("standalone")
    }
}

#[test]
fn standalone_catalog_precedes_asterisk_package_catalog_and_falls_back_to_it() {
    let directories = TemporaryCatalogs::new();
    fs::write(
        directories.standalone().join("fr-CA.ftl"),
        "no-links =\n    .text = STANDALONE\n",
    )
    .unwrap();
    fs::write(
        directories.package().join("fr-CA.ftl"),
        "no-links =\n    .text = ASTERISK\n",
    )
    .unwrap();

    let catalog = MessageCatalog::load_from_catalog_dirs(
        "fr-CA",
        &[],
        &[directories.standalone(), directories.package()],
    )
    .unwrap();
    assert_eq!(
        catalog.format(&Message::NoLinks).unwrap().text,
        "STANDALONE"
    );

    fs::remove_file(directories.standalone().join("fr-CA.ftl")).unwrap();
    let catalog = MessageCatalog::load_from_catalog_dirs(
        "fr-CA",
        &[],
        &[directories.standalone(), directories.package()],
    )
    .unwrap();
    assert_eq!(catalog.format(&Message::NoLinks).unwrap().text, "ASTERISK");
}

impl Drop for TemporaryCatalogs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn catalog_loader_prefers_admin_translation_and_uses_package_fallback() {
    let directories = TemporaryCatalogs::new();
    fs::write(directories.admin().join("en-US.ftl"), ENGLISH).unwrap();
    fs::write(directories.package().join("en-US.ftl"), ENGLISH).unwrap();
    fs::write(
        directories.admin().join("fr-CA.ftl"),
        "no-links =\n    .text = ADMIN\n",
    )
    .unwrap();
    fs::write(
        directories.package().join("fr-CA.ftl"),
        "no-links =\n    .text = PACKAGE\n",
    )
    .unwrap();

    let admin = MessageCatalog::load_from_catalog_dirs(
        "fr-CA",
        &[directories.admin()],
        &[directories.package()],
    )
    .unwrap();
    assert_eq!(admin.format(&Message::NoLinks).unwrap().text, "ADMIN");

    fs::remove_file(directories.admin().join("fr-CA.ftl")).unwrap();
    let package = MessageCatalog::load_from_catalog_dirs(
        "fr-CA",
        &[directories.admin()],
        &[directories.package()],
    )
    .unwrap();
    assert_eq!(package.format(&Message::NoLinks).unwrap().text, "PACKAGE");
}

#[test]
fn catalog_loader_uses_embedded_english_when_no_catalog_files_exist() {
    let directories = TemporaryCatalogs::new();
    let catalog = MessageCatalog::load_from_catalog_dirs(
        "fr-CA",
        &[directories.admin()],
        &[directories.package()],
    )
    .unwrap();
    assert_eq!(catalog.format(&Message::NoLinks).unwrap().text, "NO LINKS");
    assert_eq!(catalog.translation_warnings(), &["catalog unavailable"]);
}

#[test]
fn malformed_catalog_overlay_falls_back_with_one_useful_warning() {
    let directories = TemporaryCatalogs::new();
    fs::write(directories.admin().join("fr-CA.ftl"), "no-links = {\n").unwrap();
    let catalog = MessageCatalog::load_from_catalog_dirs(
        "fr-CA",
        &[directories.admin()],
        &[directories.package()],
    )
    .unwrap();
    assert_eq!(catalog.format(&Message::NoLinks).unwrap().text, "NO LINKS");
    assert_eq!(catalog.translation_warnings(), &["catalog is malformed"]);
}
