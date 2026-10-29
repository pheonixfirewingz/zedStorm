use zed_extension_api as zed;

struct JupyterExtension;

impl zed::Extension for JupyterExtension {
    fn new() -> Self {
        Self
    }
}

zed::register_extension!(JupyterExtension);
