use zed_extension_api::{self as zed};

struct DrawioExtension;

impl zed::Extension for DrawioExtension {
    fn new() -> Self {
        Self
    }
}

zed::register_extension!(DrawioExtension);
