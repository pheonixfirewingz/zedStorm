use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufReader, Read},
    path::Path,
};

use anyhow::{Context, Result};
use roxmltree::Document;

use crate::model::{
    DataTypeInfo, FieldInfo, HardwareModuleInfo, IoChannelInfo, ManifestInfo, PlcNextProject,
    PouInfo, PouType, ProjectFileInfo, TaskInfo, TaskProgramRef, VariableInfo,
};

pub struct PlcNextParser;

impl PlcNextParser {
    pub fn parse_project(path: &Path) -> Result<PlcNextProject> {
        let file_name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "PLCnext_Project".to_string());

        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        let mut project = PlcNextProject::new(file_name, path.to_string_lossy().to_string(), ext.clone());

        if path.is_dir() {
            Self::parse_from_directory(path, &mut project)?;
        } else {
            Self::parse_from_archive(path, &mut project)?;
        }

        Ok(project)
    }

    fn parse_from_directory(dir: &Path, project: &mut PlcNextProject) -> Result<()> {
        let mut file_entries = BTreeMap::new();
        Self::collect_files_recursive(dir, dir, &mut file_entries)?;

        for (rel_path, content) in &file_entries {
            project.files.push(ProjectFileInfo {
                path: rel_path.clone(),
                size_bytes: content.len() as u64,
                aspect: Self::determine_aspect(rel_path),
                pou_name: Self::extract_pou_name_from_path(rel_path),
            });
        }

        Self::process_project_content(&file_entries, project);
        Ok(())
    }

    fn collect_files_recursive(
        base: &Path,
        current: &Path,
        files: &mut BTreeMap<String, Vec<u8>>,
    ) -> Result<()> {
        if !current.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                Self::collect_files_recursive(base, &path, files)?;
            } else if path.is_file() {
                if let Ok(rel) = path.strip_prefix(base) {
                    let rel_str = rel.to_string_lossy().replace('\\', "/");
                    if let Ok(bytes) = fs::read(&path) {
                        files.insert(rel_str, bytes);
                    }
                }
            }
        }
        Ok(())
    }

    fn parse_from_archive(archive_path: &Path, project: &mut PlcNextProject) -> Result<()> {
        let file = File::open(archive_path)
            .with_context(|| format!("Failed to open archive: {}", archive_path.display()))?;
        let reader = BufReader::new(file);
        let mut zip = zip::ZipArchive::new(reader)
            .with_context(|| format!("Invalid ZIP/PLCnext archive: {}", archive_path.display()))?;

        let mut file_entries = BTreeMap::new();

        for i in 0..zip.len() {
            let mut file = zip.by_index(i)?;
            let name = file.name().replace('\\', "/");
            if file.is_dir() {
                continue;
            }
            let mut buf = Vec::new();
            let _ = file.read_to_end(&mut buf);
            let size = buf.len() as u64;

            project.files.push(ProjectFileInfo {
                path: name.clone(),
                size_bytes: size,
                aspect: Self::determine_aspect(&name),
                pou_name: Self::extract_pou_name_from_path(&name),
            });

            file_entries.insert(name, buf);
        }

        Self::process_project_content(&file_entries, project);
        Ok(())
    }

    fn process_project_content(
        files: &BTreeMap<String, Vec<u8>>,
        project: &mut PlcNextProject,
    ) {
        // 1. Process Manifest (manifest.xml)
        if let Some(manifest_bytes) = files.get("manifest.xml").or_else(|| {
            files.iter().find(|(k, _)| k.ends_with("manifest.xml")).map(|(_, v)| v)
        }) {
            if let Ok(xml_str) = std::str::from_utf8(manifest_bytes) {
                Self::parse_manifest(xml_str, &mut project.manifest);
            }
        }

        // 2. Process Structure (structure.xml)
        if let Some(structure_bytes) = files.get("structure.xml").or_else(|| {
            files.iter().find(|(k, _)| k.ends_with("structure.xml")).map(|(_, v)| v)
        }) {
            if let Ok(xml_str) = std::str::from_utf8(structure_bytes) {
                Self::parse_structure(xml_str, project);
            }
        }

        // 3. Process Content files: POUs, Variables, Data Types, Code
        // Map of POU Name -> (ST Code, Variables, Description, PouType)
        let mut pou_map: BTreeMap<String, (Option<String>, Vec<VariableInfo>, String, Option<PouType>)> = BTreeMap::new();

        for (path, bytes) in files {
            let lower = path.to_lowercase();
            let text = std::str::from_utf8(bytes).unwrap_or("");

            // Data Types (*.dt)
            if lower.ends_with(".dt") {
                Self::parse_data_types(text, &mut project.data_types);
                continue;
            }

            // Global variables (PROJECT~Variables~*.var or global.var)
            if (lower.contains("project~variables") || lower.contains("global~variables") || lower.ends_with("global.var"))
                && lower.ends_with(".var")
            {
                let mut vars = Vec::new();
                Self::parse_variables_content(text, "VAR_GLOBAL", &mut vars);
                project.global_variables.extend(vars);
                continue;
            }

            // POU specific files: <PouName>~<Aspect>~<Id>.<ext>
            if let Some(pou_name) = Self::extract_pou_name_from_path(path) {
                let entry = pou_map.entry(pou_name.clone()).or_insert_with(|| {
                    (None, Vec::new(), String::new(), None)
                });

                if lower.ends_with(".st") {
                    entry.0 = Some(text.to_string());
                } else if lower.ends_with(".var") {
                    Self::parse_variables_content(text, "VAR", &mut entry.1);
                } else if lower.ends_with(".ld") || lower.ends_with(".fbd") {
                    // Ladder or Function Block Diagram - convert networks to readable logic
                    let generated_code = Self::parse_ladder_or_fbd(text, &pou_name);
                    entry.0 = Some(generated_code);
                } else if lower.ends_with(".html") || lower.ends_with(".txt") {
                    entry.2 = text.to_string();
                } else if lower.ends_with(".meta") {
                    if let Some(ptype) = Self::parse_pou_type_from_meta(text) {
                        entry.3 = Some(ptype);
                    }
                }
            }
        }

        // Merge discovered POUs into project.pous (avoiding duplicates if structure.xml also added them)
        for (name, (code_opt, vars, desc, ptype_opt)) in pou_map {
            let code = code_opt.unwrap_or_else(|| {
                format!("// POU: {}\n// No source code worksheet available\n", name)
            });

            if let Some(existing) = project.pous.iter_mut().find(|p| p.name == name) {
                if existing.code.is_empty() || existing.code.starts_with("// POU:") {
                    existing.code = code;
                }
                if existing.variables.is_empty() {
                    existing.variables = vars;
                }
                if existing.description.is_empty() {
                    existing.description = desc;
                }
                if let Some(ptype) = ptype_opt {
                    existing.pou_type = ptype;
                }
            } else {
                let inferred_type = ptype_opt.unwrap_or_else(|| {
                    if name.starts_with("FB_") || name.ends_with("_FB") {
                        PouType::FunctionBlock
                    } else if name.starts_with("FC_") || name.ends_with("_FC") {
                        PouType::Function
                    } else {
                        PouType::Program
                    }
                });

                let lang = if code.contains("(* LD:") || code.contains("// Network") {
                    "LadderDiagram".to_string()
                } else {
                    "StructuredText".to_string()
                };

                project.pous.push(PouInfo {
                    name,
                    pou_type: inferred_type,
                    language: lang,
                    description: desc,
                    code,
                    variables: vars,
                });
            }
        }

        // If no tasks were discovered in structure.xml, create default ESM CyclicTask
        if project.tasks.is_empty() {
            let mut default_task = TaskInfo {
                name: "CyclicTask".to_string(),
                cycle_time: "T#20ms".to_string(),
                priority: 50,
                watchdog: "T#50ms".to_string(),
                task_type: "Cyclic".to_string(),
                programs: Vec::new(),
            };
            for (i, p) in project.pous.iter().filter(|p| p.pou_type == PouType::Program).enumerate() {
                default_task.programs.push(TaskProgramRef {
                    program_name: p.name.clone(),
                    instance_name: format!("{}_1", p.name),
                    execution_order: (i + 1) as u32,
                });
            }
            project.tasks.push(default_task);
        }

        // If no hardware modules were found in structure.xml, populate standard target onboard modules
        if project.hardware.is_empty() {
            Self::populate_default_hardware(&project.manifest.target, &mut project.hardware);
        }
    }

    fn parse_manifest(xml: &str, manifest: &mut ManifestInfo) {
        if let Ok(doc) = Document::parse(xml) {
            let root = doc.root_element();
            for node in root.descendants() {
                match node.tag_name().name() {
                    "Target" | "TargetName" | "Controller" => {
                        if let Some(text) = node.text() {
                            manifest.target = text.trim().to_string();
                        }
                    }
                    "Firmware" | "FirmwareVersion" => {
                        if let Some(text) = node.text() {
                            manifest.firmware = text.trim().to_string();
                        }
                    }
                    "Version" | "VersionId" | "SchemaVersion" => {
                        if let Some(text) = node.text() {
                            manifest.schema_version = text.trim().to_string();
                        }
                    }
                    "Author" | "Company" | "CreatedBy" => {
                        if let Some(text) = node.text() {
                            manifest.author = text.trim().to_string();
                        }
                    }
                    "Date" | "CreationDate" | "ModifiedDate" => {
                        if let Some(text) = node.text() {
                            manifest.date = text.trim().to_string();
                        }
                    }
                    "Guid" | "ProjectId" | "SolutionId" => {
                        if let Some(text) = node.text() {
                            manifest.guid = text.trim().to_string();
                        }
                    }
                    "Description" => {
                        if let Some(text) = node.text() {
                            manifest.description = text.trim().to_string();
                        }
                    }
                    _ => {}
                }
                // Check attributes as well
                if let Some(t) = node.attribute("Target").or_else(|| node.attribute("target")) {
                    manifest.target = t.to_string();
                }
                if let Some(f) = node.attribute("Firmware").or_else(|| node.attribute("firmware")) {
                    manifest.firmware = f.to_string();
                }
                if let Some(v) = node.attribute("Version").or_else(|| node.attribute("VersionId")) {
                    manifest.schema_version = v.to_string();
                }
            }
        }
    }

    fn parse_structure(xml: &str, project: &mut PlcNextProject) {
        if let Ok(doc) = Document::parse(xml) {
            let root = doc.root_element();

            for node in root.descendants() {
                let tag = node.tag_name().name();

                // Tasks (ESM)
                if tag == "Task" || tag == "EsmTask" || tag == "CyclicTask" {
                    let name = node.attribute("Name").or_else(|| node.attribute("name")).unwrap_or("Task");
                    let cycle = node.attribute("CycleTime").or_else(|| node.attribute("cycleTime")).unwrap_or("T#20ms");
                    let priority: u32 = node.attribute("Priority").and_then(|p| p.parse().ok()).unwrap_or(50);
                    let watchdog = node.attribute("Watchdog").unwrap_or("T#50ms");
                    let task_type = node.attribute("Type").unwrap_or("Cyclic");

                    let mut program_refs = Vec::new();
                    for child in node.children() {
                        if child.tag_name().name() == "Program" || child.tag_name().name() == "ProgramRef" || child.tag_name().name() == "Instance" {
                            let prog_name = child.attribute("Name").or_else(|| child.attribute("ProgramName")).unwrap_or("");
                            let inst_name = child.attribute("InstanceName").unwrap_or(prog_name);
                            let order = child.attribute("Order").and_then(|o| o.parse().ok()).unwrap_or(program_refs.len() as u32 + 1);
                            if !prog_name.is_empty() {
                                program_refs.push(TaskProgramRef {
                                    program_name: prog_name.to_string(),
                                    instance_name: inst_name.to_string(),
                                    execution_order: order,
                                });
                            }
                        }
                    }

                    project.tasks.push(TaskInfo {
                        name: name.to_string(),
                        cycle_time: cycle.to_string(),
                        priority,
                        watchdog: watchdog.to_string(),
                        task_type: task_type.to_string(),
                        programs: program_refs,
                    });
                }

                // POUs defined in structure
                if tag == "Program" || tag == "FunctionBlock" || tag == "Function" {
                    if let Some(name) = node.attribute("Name") {
                        let pou_type = match tag {
                            "Program" => PouType::Program,
                            "FunctionBlock" => PouType::FunctionBlock,
                            "Function" => PouType::Function,
                            _ => PouType::Program,
                        };
                        let lang = node.attribute("Language").unwrap_or("StructuredText").to_string();
                        let desc = node.attribute("Description").unwrap_or("").to_string();

                        if !project.pous.iter().any(|p| p.name == name) {
                            project.pous.push(PouInfo {
                                name: name.to_string(),
                                pou_type,
                                language: lang,
                                description: desc,
                                code: String::new(),
                                variables: Vec::new(),
                            });
                        }
                    }
                }

                // Hardware modules
                if tag == "Module" || tag == "Device" || tag == "AxiolineModule" || tag == "Terminal" {
                    let name = node.attribute("Name").or_else(|| node.attribute("Type")).unwrap_or("Module");
                    let catalog = node.attribute("OrderNumber").or_else(|| node.attribute("ArticleNumber")).unwrap_or("");
                    let desc = node.attribute("Description").unwrap_or("");
                    let slot: u32 = node.attribute("Slot").or_else(|| node.attribute("Position")).and_then(|s| s.parse().ok()).unwrap_or(project.hardware.len() as u32);

                    let mut channels = Vec::new();
                    for child in node.children() {
                        if child.tag_name().name() == "Channel" || child.tag_name().name() == "Port" {
                            let ch_name = child.attribute("Name").unwrap_or("IN");
                            let ch_idx = child.attribute("Index").and_then(|i| i.parse().ok()).unwrap_or(channels.len() as u32);
                            let dir = child.attribute("Direction").unwrap_or("Input");
                            let dtype = child.attribute("DataType").unwrap_or("BOOL");
                            let mapped = child.attribute("Variable").map(|s| s.to_string());

                            channels.push(IoChannelInfo {
                                channel: ch_idx,
                                name: ch_name.to_string(),
                                direction: dir.to_string(),
                                data_type: dtype.to_string(),
                                mapped_symbol: mapped,
                            });
                        }
                    }

                    project.hardware.push(HardwareModuleInfo {
                        slot,
                        name: name.to_string(),
                        catalog_number: catalog.to_string(),
                        description: desc.to_string(),
                        channels,
                    });
                }
            }
        }
    }

    fn parse_variables_content(text: &str, default_scope: &str, vars: &mut Vec<VariableInfo>) {
        if text.trim().starts_with('<') {
            if let Ok(doc) = Document::parse(text) {
                let root = doc.root_element();
                for node in root.descendants() {
                    if node.tag_name().name() == "Variable" || node.tag_name().name() == "Var" {
                        let name = node.attribute("Name").or_else(|| node.attribute("name")).unwrap_or("");
                        if name.is_empty() {
                            continue;
                        }
                        let data_type = node.attribute("Type").or_else(|| node.attribute("type")).or_else(|| node.attribute("DataType")).unwrap_or("BOOL");
                        let scope = node.attribute("Usage").or_else(|| node.attribute("Direction")).or_else(|| node.attribute("Scope")).unwrap_or(default_scope);
                        let init = node.attribute("InitialValue").or_else(|| node.attribute("init")).map(|s| s.to_string());
                        let is_port = node.attribute("Port").map(|s| s == "true" || s == "1").unwrap_or(false);
                        let is_retain = node.attribute("Retain").map(|s| s == "true" || s == "1").unwrap_or(false);
                        let is_const = node.attribute("Constant").map(|s| s == "true" || s == "1").unwrap_or(false);

                        let comment = node
                            .children()
                            .find(|c| c.tag_name().name() == "Comment" || c.tag_name().name() == "Documentation")
                            .and_then(|c| c.text())
                            .or_else(|| node.attribute("Comment"))
                            .unwrap_or("")
                            .trim()
                            .to_string();

                        vars.push(VariableInfo {
                            name: name.to_string(),
                            data_type: data_type.to_string(),
                            scope: Self::normalize_scope(scope),
                            initial_value: init,
                            is_port,
                            is_retain,
                            is_constant: is_const,
                            comment,
                        });
                    }
                }
                return;
            }
        }

        // Fallback: parse plain text IEC 61131-3 variable blocks
        Self::parse_iec_variable_block(text, default_scope, vars);
    }

    fn parse_iec_variable_block(text: &str, default_scope: &str, vars: &mut Vec<VariableInfo>) {
        let mut current_scope = default_scope.to_string();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("(*") {
                continue;
            }
            let upper = trimmed.to_uppercase();
            if upper.starts_with("VAR_INPUT") {
                current_scope = "VAR_INPUT".to_string();
                continue;
            } else if upper.starts_with("VAR_OUTPUT") {
                current_scope = "VAR_OUTPUT".to_string();
                continue;
            } else if upper.starts_with("VAR_IN_OUT") {
                current_scope = "VAR_IN_OUT".to_string();
                continue;
            } else if upper.starts_with("VAR_GLOBAL") {
                current_scope = "VAR_GLOBAL".to_string();
                continue;
            } else if upper.starts_with("VAR_EXTERNAL") {
                current_scope = "VAR_EXTERNAL".to_string();
                continue;
            } else if upper.starts_with("VAR_TEMP") {
                current_scope = "VAR_TEMP".to_string();
                continue;
            } else if upper == "VAR" {
                current_scope = "VAR".to_string();
                continue;
            } else if upper.starts_with("END_VAR") {
                continue;
            }

            // e.g. "bStart : BOOL := FALSE; // Start request"
            if let Some(colon_idx) = trimmed.find(':') {
                let name = trimmed[..colon_idx].trim();
                let remainder = trimmed[colon_idx + 1..].trim();
                let (type_and_init, comment) = if let Some(comm_idx) = remainder.find("//") {
                    (&remainder[..comm_idx], remainder[comm_idx + 2..].trim())
                } else if let Some(comm_idx) = remainder.find("(*") {
                    let end_comm = remainder.find("*)").unwrap_or(remainder.len());
                    (&remainder[..comm_idx], remainder[comm_idx + 2..end_comm].trim())
                } else {
                    (remainder, "")
                };

                let clean = type_and_init.trim_end_matches(';').trim();
                let (dtype, init_val) = if let Some(assign_idx) = clean.find(":=") {
                    (clean[..assign_idx].trim(), Some(clean[assign_idx + 2..].trim().to_string()))
                } else {
                    (clean, None)
                };

                if !name.is_empty() && !dtype.is_empty() {
                    vars.push(VariableInfo {
                        name: name.to_string(),
                        data_type: dtype.to_string(),
                        scope: current_scope.clone(),
                        initial_value: init_val,
                        is_port: false,
                        is_retain: current_scope.contains("RETAIN"),
                        is_constant: current_scope.contains("CONSTANT"),
                        comment: comment.to_string(),
                    });
                }
            }
        }
    }

    fn parse_data_types(text: &str, data_types: &mut Vec<DataTypeInfo>) {
        if text.trim().starts_with('<') {
            if let Ok(doc) = Document::parse(text) {
                let root = doc.root_element();
                for node in root.descendants() {
                    if node.tag_name().name() == "DataType" || node.tag_name().name() == "Type" {
                        let name = node.attribute("Name").unwrap_or("UnnamedType");
                        let comment = node.attribute("Comment").unwrap_or("").to_string();

                        let mut fields = Vec::new();
                        let mut enum_values = Vec::new();
                        let mut kind = "Alias".to_string();

                        for child in node.children() {
                            let ctag = child.tag_name().name();
                            if ctag == "Struct" || ctag == "Structure" {
                                kind = "Struct".to_string();
                                for f in child.children() {
                                    if f.tag_name().name() == "Field" || f.tag_name().name() == "Member" {
                                        let fname = f.attribute("Name").unwrap_or("");
                                        let ftype = f.attribute("Type").unwrap_or("BOOL");
                                        let finital = f.attribute("InitialValue").map(|s| s.to_string());
                                        let fcomment = f.attribute("Comment").unwrap_or("").to_string();
                                        fields.push(FieldInfo {
                                            name: fname.to_string(),
                                            data_type: ftype.to_string(),
                                            initial_value: finital,
                                            comment: fcomment,
                                        });
                                    }
                                }
                            } else if ctag == "Enum" || ctag == "Enumeration" {
                                kind = "Enum".to_string();
                                for ev in child.children() {
                                    if ev.tag_name().name() == "Value" || ev.tag_name().name() == "Item" {
                                        let val_name = ev.attribute("Name").unwrap_or("");
                                        if !val_name.is_empty() {
                                            enum_values.push(val_name.to_string());
                                        }
                                    }
                                }
                            }
                        }

                        data_types.push(DataTypeInfo {
                            name: name.to_string(),
                            kind,
                            fields,
                            enum_values,
                            raw_def: text.to_string(),
                            comment,
                        });
                    }
                }
                return;
            }
        }

        // Plain text IEC 61131-3 TYPE declarations
        let mut cur_name = String::new();
        let mut cur_kind = "Struct".to_string();
        let mut cur_fields = Vec::new();
        let mut cur_enums = Vec::new();
        let mut in_type = false;

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("TYPE") {
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() >= 2 {
                    cur_name = parts[1].trim_end_matches(':').to_string();
                    in_type = true;
                    cur_fields.clear();
                    cur_enums.clear();
                }
            } else if trimmed.starts_with("STRUCT") {
                cur_kind = "Struct".to_string();
            } else if trimmed.starts_with("(") && in_type {
                cur_kind = "Enum".to_string();
                let inner = trimmed.trim_matches(|c| c == '(' || c == ')' || c == ';');
                for item in inner.split(',') {
                    cur_enums.push(item.trim().to_string());
                }
            } else if trimmed.starts_with("END_TYPE") {
                if !cur_name.is_empty() {
                    data_types.push(DataTypeInfo {
                        name: cur_name.clone(),
                        kind: cur_kind.clone(),
                        fields: cur_fields.clone(),
                        enum_values: cur_enums.clone(),
                        raw_def: text.to_string(),
                        comment: String::new(),
                    });
                }
                in_type = false;
            } else if in_type && cur_kind == "Struct" && trimmed.contains(':') {
                let parts: Vec<&str> = trimmed.split(':').collect();
                let fname = parts[0].trim();
                let ftype = parts[1].trim().trim_end_matches(';').trim();
                cur_fields.push(FieldInfo {
                    name: fname.to_string(),
                    data_type: ftype.to_string(),
                    initial_value: None,
                    comment: String::new(),
                });
            }
        }
    }

    fn parse_ladder_or_fbd(xml: &str, pou_name: &str) -> String {
        let mut out = format!("// Ladder / FBD Graphical Worksheet for {}\n// Rendered from PLCnext XML Network Schema\n\n", pou_name);
        if let Ok(doc) = Document::parse(xml) {
            let mut network_count = 0;
            for node in doc.root_element().descendants() {
                if node.tag_name().name() == "Network" || node.tag_name().name() == "Rung" {
                    network_count += 1;
                    let comment = node.attribute("Comment").unwrap_or("Logic Network");
                    let title = node.attribute("Title").unwrap_or("");
                    out.push_str(&format!("// ==========================================\n// Network {}: {} {}\n// ==========================================\n", network_count, title, comment));

                    // Inspect contacts, coils, and blocks in the network
                    for element in node.descendants() {
                        let el_tag = element.tag_name().name();
                        let var_name = element.attribute("Variable").or_else(|| element.attribute("Name")).unwrap_or("");
                        let operand = element.attribute("Operand").unwrap_or("");
                        let target = if !var_name.is_empty() { var_name } else { operand };

                        match el_tag {
                            "Contact" => {
                                let negated = element.attribute("Negated").map(|s| s == "true").unwrap_or(false);
                                if negated {
                                    out.push_str(&format!("    --[/]-- {} (Normally Closed)\n", target));
                                } else {
                                    out.push_str(&format!("    --[ ]-- {} (Normally Open)\n", target));
                                }
                            }
                            "Coil" => {
                                let coil_type = element.attribute("Type").unwrap_or("Standard");
                                out.push_str(&format!("    --( )-- {} [Coil: {}]\n", target, coil_type));
                            }
                            "Block" | "FunctionBlock" => {
                                let fb_type = element.attribute("Type").unwrap_or("BLOCK");
                                out.push_str(&format!("    +--[{}] ({})--+\n", fb_type, target));
                            }
                            _ => {}
                        }
                    }
                    out.push('\n');
                }
            }
            if network_count > 0 {
                return out;
            }
        }

        format!("// Graphical logic diagram for POU: {}\n// Contains {} bytes of XML network definitions.\n", pou_name, xml.len())
    }

    fn parse_pou_type_from_meta(text: &str) -> Option<PouType> {
        if let Ok(doc) = Document::parse(text) {
            let root = doc.root_element();
            for node in root.descendants() {
                if let Some(t) = node.attribute("Type").or_else(|| node.attribute("PouType")) {
                    match t.to_lowercase().as_str() {
                        "program" => return Some(PouType::Program),
                        "functionblock" | "function_block" => return Some(PouType::FunctionBlock),
                        "function" => return Some(PouType::Function),
                        _ => {}
                    }
                }
            }
        }
        None
    }

    fn populate_default_hardware(target: &str, hardware: &mut Vec<HardwareModuleInfo>) {
        hardware.push(HardwareModuleInfo {
            slot: 0,
            name: format!("{target} (Controller CPU & Onboard I/O)"),
            catalog_number: "2404267".to_string(),
            description: "PLCnext Control for direct control of Axioline F I/O".to_string(),
            channels: vec![
                IoChannelInfo { channel: 0, name: "IN00".to_string(), direction: "Input".to_string(), data_type: "BOOL".to_string(), mapped_symbol: Some("bEmergencyStop".to_string()) },
                IoChannelInfo { channel: 1, name: "IN01".to_string(), direction: "Input".to_string(), data_type: "BOOL".to_string(), mapped_symbol: Some("bStartButton".to_string()) },
                IoChannelInfo { channel: 2, name: "IN02".to_string(), direction: "Input".to_string(), data_type: "BOOL".to_string(), mapped_symbol: Some("bStopButton".to_string()) },
                IoChannelInfo { channel: 3, name: "IN03".to_string(), direction: "Input".to_string(), data_type: "BOOL".to_string(), mapped_symbol: Some("bResetFault".to_string()) },
                IoChannelInfo { channel: 4, name: "OUT00".to_string(), direction: "Output".to_string(), data_type: "BOOL".to_string(), mapped_symbol: Some("bSystemReadyLed".to_string()) },
                IoChannelInfo { channel: 5, name: "OUT01".to_string(), direction: "Output".to_string(), data_type: "BOOL".to_string(), mapped_symbol: Some("bAlarmHorn".to_string()) },
            ],
        });

        hardware.push(HardwareModuleInfo {
            slot: 1,
            name: "AXL F DI16/1 1H".to_string(),
            catalog_number: "2688310".to_string(),
            description: "Axioline F, Digital input module, 16 inputs, 24 V DC".to_string(),
            channels: (0..16)
                .map(|i| IoChannelInfo {
                    channel: i,
                    name: format!("DI_{:02}", i),
                    direction: "Input".to_string(),
                    data_type: "BOOL".to_string(),
                    mapped_symbol: Some(format!("Sensor_Rack1_In_{:02}", i)),
                })
                .collect(),
        });

        hardware.push(HardwareModuleInfo {
            slot: 2,
            name: "AXL F DO16/1 1H".to_string(),
            catalog_number: "2688349".to_string(),
            description: "Axioline F, Digital output module, 16 outputs, 24 V DC, 500 mA".to_string(),
            channels: (0..16)
                .map(|i| IoChannelInfo {
                    channel: i,
                    name: format!("DO_{:02}", i),
                    direction: "Output".to_string(),
                    data_type: "BOOL".to_string(),
                    mapped_symbol: Some(format!("Actuator_Rack1_Out_{:02}", i)),
                })
                .collect(),
        });

        hardware.push(HardwareModuleInfo {
            slot: 3,
            name: "AXL F AI4 I 1H".to_string(),
            catalog_number: "2688491".to_string(),
            description: "Axioline F, Analog input module, 4 analog inputs, 0-20 mA / 4-20 mA".to_string(),
            channels: (0..4)
                .map(|i| IoChannelInfo {
                    channel: i,
                    name: format!("AI_{:02}", i),
                    direction: "Input".to_string(),
                    data_type: "INT".to_string(),
                    mapped_symbol: Some(format!("AnalogSensor_Ch{:02}", i)),
                })
                .collect(),
        });
    }

    fn determine_aspect(path: &str) -> String {
        let p = path.to_lowercase();
        if p.ends_with(".st") {
            "Structured Text Code".to_string()
        } else if p.ends_with(".var") {
            "Variable Declarations".to_string()
        } else if p.ends_with(".dt") {
            "User Defined Types".to_string()
        } else if p.ends_with(".ld") {
            "Ladder Logic".to_string()
        } else if p.ends_with(".fbd") {
            "Function Block Diagram".to_string()
        } else if p.ends_with(".meta") {
            "POU Metadata".to_string()
        } else if p.ends_with(".html") {
            "POU Documentation".to_string()
        } else if p.ends_with(".proj") {
            "Project Configuration".to_string()
        } else if p.ends_with("structure.xml") {
            "Solution Structure".to_string()
        } else if p.ends_with("manifest.xml") {
            "Project Manifest".to_string()
        } else {
            "Resource File".to_string()
        }
    }

    fn extract_pou_name_from_path(path: &str) -> Option<String> {
        let p = path.replace('\\', "/");
        let filename = p.split('/').last()?;
        if let Some(pos) = filename.find('~') {
            let name = &filename[..pos];
            if name != "PROJECT" && name != "Global" && name != "Types" {
                return Some(name.to_string());
            }
        }
        None
    }

    fn normalize_scope(scope: &str) -> String {
        let s = scope.to_uppercase();
        if s.contains("INPUT") {
            "VAR_INPUT".to_string()
        } else if s.contains("OUTPUT") {
            "VAR_OUTPUT".to_string()
        } else if s.contains("IN_OUT") || s.contains("INOUT") {
            "VAR_IN_OUT".to_string()
        } else if s.contains("GLOBAL") {
            "VAR_GLOBAL".to_string()
        } else if s.contains("EXTERNAL") {
            "VAR_EXTERNAL".to_string()
        } else if s.contains("TEMP") {
            "VAR_TEMP".to_string()
        } else {
            "VAR".to_string()
        }
    }
}
