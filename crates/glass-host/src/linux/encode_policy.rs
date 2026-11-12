//! Selection and failures belong to one monitor, not the whole host.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Vaapi,
    Nvenc,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Vaapi => "VA-API",
            Self::Nvenc => "NVENC",
        }
    }
}

pub struct Policy {
    candidates: &'static [Backend],
    failed: Vec<Backend>,
    source: Option<(usize, usize)>,
}

impl Policy {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let candidates: &'static [Backend] = match value.to_ascii_lowercase().as_str() {
            "" | "software" => &[],
            "vaapi" => &[Backend::Vaapi],
            "nvenc" => &[Backend::Nvenc],
            "auto" => &[Backend::Nvenc, Backend::Vaapi],
            _ => anyhow::bail!(
                "unknown GLASS_ENCODER={value:?}; expected software, vaapi, nvenc, or auto"
            ),
        };
        Ok(Self {
            candidates,
            failed: Vec::new(),
            source: None,
        })
    }

    pub fn next(&self) -> Option<Backend> {
        self.candidates
            .iter()
            .copied()
            .find(|b| !self.failed.contains(b))
    }

    pub fn source_changed(&mut self, width: usize, height: usize) {
        if self.source != Some((width, height)) {
            self.source = Some((width, height));
            self.failed.clear();
        }
    }

    pub fn fail(&mut self, backend: Backend) {
        if !self.failed.contains(&backend) {
            self.failed.push(backend);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_nvenc_never_tries_vaapi() {
        let mut p = Policy::parse("NVENC").unwrap();
        assert_eq!(p.next(), Some(Backend::Nvenc));
        p.fail(Backend::Nvenc);
        assert_eq!(p.next(), None);
        assert!(Policy::parse("typo").is_err());
        assert_eq!(Policy::parse("").unwrap().next(), None);
    }

    #[test]
    fn failures_are_local_and_auto_advances() {
        let mut first = Policy::parse("auto").unwrap();
        first.source_changed(1920, 1080);
        let second = Policy::parse("auto").unwrap();
        first.fail(Backend::Nvenc);
        assert_eq!(first.next(), Some(Backend::Vaapi));
        assert_eq!(second.next(), Some(Backend::Nvenc));
        first.fail(Backend::Vaapi);
        assert_eq!(first.next(), None);
        first.source_changed(1920, 1080);
        assert_eq!(first.next(), None, "same configuration must not retry");
        first.source_changed(1280, 720);
        assert_eq!(first.next(), Some(Backend::Nvenc));
    }
}
