use std::path::{Path, PathBuf};
use anyhow::Result;
use serde_json::Value;

use crate::model::PlcNextProject;
use crate::parser::PlcNextParser;

pub struct Document {
    path: PathBuf,
    project: PlcNextProject,
}

impl Document {
    pub fn open(path: &Path) -> Result<Self> {
        let project = PlcNextParser::parse_project(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            project,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn project(&self) -> &PlcNextProject {
        &self.project
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(&self.project).unwrap_or(Value::Null)
    }

    pub fn export_all_st(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "(* =========================================================================\n   PROJECT: {}\n   TARGET:  {} (Firmware: {})\n   EXPORT:  All IEC 61131-3 Structured Text POUs\n   ========================================================================= *)\n\n",
            self.project.project_name, self.project.manifest.target, self.project.manifest.firmware
        ));

        for pou in &self.project.pous {
            out.push_str(&format!(
                "(* -------------------------------------------------------------------------\n   POU:  {} [{}]\n   LANG: {}\n   ------------------------------------------------------------------------- *)\n",
                pou.name, pou.pou_type, pou.language
            ));

            if !pou.variables.is_empty() {
                out.push_str(&Self::format_variables_as_iec(&pou.variables));
            }

            out.push_str(&pou.code);
            out.push_str("\n\n");
        }

        out
    }

    pub fn export_variables_csv(&self) -> String {
        let mut csv = String::from("POU,Scope,Name,DataType,InitialValue,IsPort(GDS),Retain,Constant,Comment\n");

        for var in &self.project.global_variables {
            csv.push_str(&format!(
                "GLOBAL,{},{},{},{},{},{},{},\"{}\"\n",
                var.scope,
                var.name,
                var.data_type,
                var.initial_value.as_deref().unwrap_or(""),
                var.is_port,
                var.is_retain,
                var.is_constant,
                var.comment.replace('"', "\"\"")
            ));
        }

        for pou in &self.project.pous {
            for var in &pou.variables {
                csv.push_str(&format!(
                    "{},{},{},{},{},{},{},{},\"{}\"\n",
                    pou.name,
                    var.scope,
                    var.name,
                    var.data_type,
                    var.initial_value.as_deref().unwrap_or(""),
                    var.is_port,
                    var.is_retain,
                    var.is_constant,
                    var.comment.replace('"', "\"\"")
                ));
            }
        }

        csv
    }

    fn format_variables_as_iec(vars: &[crate::model::VariableInfo]) -> String {
        let mut out = String::new();
        let scopes = ["VAR_INPUT", "VAR_OUTPUT", "VAR_IN_OUT", "VAR", "VAR_TEMP", "VAR_EXTERNAL", "VAR_GLOBAL"];

        for scope in scopes {
            let matching: Vec<_> = vars.iter().filter(|v| v.scope == scope).collect();
            if !matching.is_empty() {
                out.push_str(&format!("{scope}\n"));
                for v in matching {
                    let port_flag = if v.is_port { " {GDS_Port}" } else { "" };
                    let retain_flag = if v.is_retain { " RETAIN" } else { "" };
                    let const_flag = if v.is_constant { " CONSTANT" } else { "" };
                    let init_val = if let Some(ref init) = v.initial_value {
                        format!(" := {}", init)
                    } else {
                        String::new()
                    };
                    let comment = if !v.comment.is_empty() {
                        format!(" // {}", v.comment)
                    } else {
                        String::new()
                    };

                    out.push_str(&format!(
                        "    {}{}{}{}: {}{};{}\n",
                        v.name, retain_flag, const_flag, port_flag, v.data_type, init_val, comment
                    ));
                }
                out.push_str("END_VAR\n\n");
            }
        }

        out
    }
}
