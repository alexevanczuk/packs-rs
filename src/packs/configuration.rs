use std::path::PathBuf;

pub struct Configuration {
    pub include: glob::Paths,
    pub absolute_root: PathBuf,
}
impl Configuration {
    fn default(absolute_root: PathBuf) -> Configuration {
        let pattern = absolute_root.join("packs/**/*.rb");
        let include = glob::glob(pattern.to_str().unwrap())
            .expect("Failed to read glob pattern");

        Configuration {
            include,
            absolute_root,
        }
    }
}

pub(crate) fn get(absolute_root: PathBuf) -> Configuration {
    Configuration::default(absolute_root)
}

#[cfg(test)]
mod tests {
    use glob::Paths;
    use itertools::Itertools;

    use super::*;
    use crate::packs::configuration;

    fn unroll_include(include: Paths) -> Vec<PathBuf> {
        include
            .map(|p| {
                p.unwrap_or_else(|err| panic!("Could not read file: {:?}", err))
            })
            .sorted()
            .collect()
    }

    #[test]
    fn default_options() {
        let absolute_root = PathBuf::from("tests/fixtures/simple_app");
        let actual = configuration::get(absolute_root.clone());
        assert_eq!(actual.absolute_root, absolute_root);
        assert_eq!(
            unroll_include(actual.include),
            vec![
                absolute_root.join("packs/bar/app/services/bar.rb"),
                absolute_root.join("packs/foo/app/services/foo.rb")
            ]
        )
    }
}
