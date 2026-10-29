use std::io::Write;
use anyhow::Result;
use zip::write::SimpleFileOptions;
use zed_plcnext_viewer::Document;

#[test]
fn test_parse_manifest_xml() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<Manifest VersionId="2024.0.1">
    <Target>AXC F 2152</Target>
    <Firmware>2024.0 LTS</Firmware>
    <Author>Phoenix Automation Labs</Author>
    <Description>Sorting Cell Solution</Description>
</Manifest>"#;

    let _doc = roxmltree::Document::parse(xml).unwrap();
    assert!(xml.contains("AXC F 2152"));
    assert!(xml.contains("2024.0 LTS"));
}

#[test]
fn test_parse_archive_zip_and_export() -> Result<()> {
    let temp = tempfile::NamedTempFile::new()?;
    let path = temp.path().to_path_buf();

    // Build sample .pcwex ZIP archive
    {
        let file = std::fs::File::create(&path)?;
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();

        zip.start_file("manifest.xml", options)?;
        zip.write_all(b"<Manifest><Target>AXC F 3152</Target><Firmware>2024.6</Firmware></Manifest>")?;

        zip.start_file("structure.xml", options)?;
        zip.write_all(b"<Solution><Task Name=\"MainTask\" CycleTime=\"T#20ms\" Priority=\"50\" /></Solution>")?;

        zip.start_file("content/MainProg~Code~0000.st", options)?;
        zip.write_all(b"IF bRun THEN\n    nCounter := nCounter + 1;\nEND_IF;\n")?;

        zip.start_file("content/MainProg~Variables~0000.var", options)?;
        zip.write_all(b"<Variables><Variable Name=\"bRun\" Type=\"BOOL\" Usage=\"Input\" /></Variables>")?;

        zip.start_file("content/Types~GeneratedD~0000.dt", options)?;
        zip.write_all(b"<DataTypes><DataType Name=\"E_MotorMode\"><Enum><Value Name=\"Manual\"/><Value Name=\"Auto\"/></Enum></DataType></DataTypes>")?;

        zip.finish()?;
    }

    let doc = Document::open(&path)?;
    let proj = doc.project();
    assert_eq!(proj.manifest.target, "AXC F 3152");
    assert_eq!(proj.manifest.firmware, "2024.6");
    assert_eq!(proj.tasks.len(), 1);
    assert_eq!(proj.tasks[0].name, "MainTask");

    let pou = proj.pous.iter().find(|p| p.name == "MainProg").expect("MainProg found");
    assert!(pou.code.contains("nCounter := nCounter + 1"));
    assert_eq!(pou.variables.len(), 1);
    assert_eq!(pou.variables[0].name, "bRun");
    assert_eq!(pou.variables[0].scope, "VAR_INPUT");

    assert_eq!(proj.data_types.len(), 1);
    assert_eq!(proj.data_types[0].name, "E_MotorMode");
    assert_eq!(proj.data_types[0].kind, "Enum");

    let exported_st = doc.export_all_st();
    assert!(exported_st.contains("POU:  MainProg"));
    assert!(exported_st.contains("nCounter := nCounter + 1"));

    let exported_csv = doc.export_variables_csv();
    assert!(exported_csv.contains("MainProg,VAR_INPUT,bRun,BOOL"));

    Ok(())
}
