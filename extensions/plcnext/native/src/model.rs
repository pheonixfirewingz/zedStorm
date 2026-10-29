use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestInfo {
    pub schema_version: String,
    pub target: String,
    pub firmware: String,
    pub author: String,
    pub date: String,
    pub guid: String,
    pub description: String,
}

impl Default for ManifestInfo {
    fn default() -> Self {
        Self {
            schema_version: "1.0".to_string(),
            target: "AXC F 2152".to_string(),
            firmware: "2024.0 LTS".to_string(),
            author: "Phoenix Contact Engineering".to_string(),
            date: "".to_string(),
            guid: "".to_string(),
            description: "".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PouType {
    Program,
    FunctionBlock,
    Function,
}

impl std::fmt::Display for PouType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PouType::Program => write!(f, "Program"),
            PouType::FunctionBlock => write!(f, "Function Block"),
            PouType::Function => write!(f, "Function"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableInfo {
    pub name: String,
    pub data_type: String,
    pub scope: String, // VAR, VAR_INPUT, VAR_OUTPUT, VAR_IN_OUT, VAR_GLOBAL, VAR_EXTERNAL, VAR_TEMP
    pub initial_value: Option<String>,
    pub is_port: bool, // GDS port attribute in PLCnext
    pub is_retain: bool,
    pub is_constant: bool,
    pub comment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PouInfo {
    pub name: String,
    pub pou_type: PouType,
    pub language: String, // StructuredText, LadderDiagram, FunctionBlockDiagram
    pub description: String,
    pub code: String,
    pub variables: Vec<VariableInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskProgramRef {
    pub program_name: String,
    pub instance_name: String,
    pub execution_order: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub name: String,
    pub cycle_time: String, // e.g. "T#20ms", "20ms"
    pub priority: u32,      // e.g. 50
    pub watchdog: String,   // e.g. "T#50ms"
    pub task_type: String,  // Cyclic, Event
    pub programs: Vec<TaskProgramRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldInfo {
    pub name: String,
    pub data_type: String,
    pub initial_value: Option<String>,
    pub comment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataTypeInfo {
    pub name: String,
    pub kind: String, // Struct, Enum, Alias
    pub fields: Vec<FieldInfo>,
    pub enum_values: Vec<String>,
    pub raw_def: String,
    pub comment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoChannelInfo {
    pub channel: u32,
    pub name: String,
    pub direction: String, // Input, Output
    pub data_type: String,
    pub mapped_symbol: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareModuleInfo {
    pub slot: u32,
    pub name: String,
    pub catalog_number: String,
    pub description: String,
    pub channels: Vec<IoChannelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectFileInfo {
    pub path: String,
    pub size_bytes: u64,
    pub aspect: String, // Code, Variables, Metadata, Structure, Manifest, etc.
    pub pou_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlcNextProject {
    pub project_name: String,
    pub file_path: String,
    pub file_type: String, // pcwex, pcweax, pcwlx, pcwef, dir
    pub manifest: ManifestInfo,
    pub tasks: Vec<TaskInfo>,
    pub pous: Vec<PouInfo>,
    pub global_variables: Vec<VariableInfo>,
    pub data_types: Vec<DataTypeInfo>,
    pub hardware: Vec<HardwareModuleInfo>,
    pub files: Vec<ProjectFileInfo>,
}

impl PlcNextProject {
    pub fn new(name: String, file_path: String, file_type: String) -> Self {
        Self {
            project_name: name,
            file_path,
            file_type,
            manifest: ManifestInfo::default(),
            tasks: Vec::new(),
            pous: Vec::new(),
            global_variables: Vec::new(),
            data_types: Vec::new(),
            hardware: Vec::new(),
            files: Vec::new(),
        }
    }
}
