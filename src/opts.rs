use serde_derive::Deserialize;
use std::{collections::HashMap, error::Error as StdError, path::PathBuf, str::FromStr};
use structopt::{
    clap::{Error, ErrorKind},
    StructOpt,
};
use structopt_toml::StructOptToml;

use crate::{Backend, Dictionary};

#[derive(Debug, Deserialize)]
struct TOMLTables {
    #[serde(rename = "backend")]
    backends: Option<Vec<Backend>>,
    #[serde(rename = "dictionary")]
    dictionaries: Option<Vec<Dictionary>>,
}

/// ⏱️  A local Fastly Compute@Edge runtime emulator
#[derive(Debug, Deserialize, StructOpt, StructOptToml)]
#[serde(default)]
pub struct Opts {
    /// Path to a Fastly Compute@Edge .wasm file
    #[structopt(long, short, default_value = "bin/main.wasm")]
    pub(crate) wasm: PathBuf,
    /// Port to listen on
    #[structopt(long, short, default_value = "3000")]
    pub(crate) port: u16,
    #[structopt(long)]
    pub(crate) tls_cert: Option<PathBuf>,
    #[structopt(long)]
    pub(crate) tls_key: Option<PathBuf>,
    /// Watch for changes to .wasm file, reloading application when relevant
    #[structopt(long)]
    pub(crate) watch: bool,
    /// TOML file to load configuration from. Commandline parameters will override
    /// the file, except for backends and dictionaries, which will be merged
    #[structopt(long, short)]
    // Ignore config_file in TOML, because we don't support daisy chaining them
    #[serde(skip)]
    pub(crate) config_file: Option<PathBuf>,
    // For TOML, tables must go last
    /// Backend to proxy in backend-name:host format (foo:foo.org)
    #[structopt(name="backend", long, short, parse(try_from_str = parse_backend))]
    #[serde(rename = "backend")]
    pub(crate) backends: Option<Vec<Backend>>,
    /// Edge dictionary in dictionary-name:key=value,key=value format
    #[structopt(name="dictionary", long, short, parse(try_from_str = parse_dictionary))]
    #[serde(rename = "dictionary")]
    pub(crate) dictionaries: Option<Vec<Dictionary>>,
}

impl Opts {
    pub(crate) fn merge_from_args_and_toml() -> Opts {
        let mut args = Opts::from_args();
        if let Some(config_file) = &args.config_file {
            let toml_string = std::fs::read_to_string(config_file).unwrap_or_else(|e| {
                // using clap's Error through StructOpt to have consistent error formatting
                Error::with_description(
                    &format!("Failed to read config file: {}", (e)),
                    ErrorKind::EmptyValue,
                )
                .exit()
            });
            let mut combined = Opts::from_args_with_toml(&toml_string).unwrap_or_else(|e| {
                Error::with_description(
                    &format!("Failed to parse config file: {}", (e)),
                    ErrorKind::EmptyValue,
                )
                .exit()
            });
            // We can't load a whole Opts straight from TOML using Serde Derive, unfortunately,
            // because then certain things are no longer optional. StructOpt-TOML normally
            // takes care of that, but it uses some hefty magic to juggle defaults around.
            // So instead, just load a struct that only has the two tables that we want to merge.
            let mut toml_tables = toml::from_str::<TOMLTables>(&toml_string).unwrap();
            // If backends is None for either, structopt-toml does the right thing, only
            // if they're both Some(), do we need to get fancy. We'll let the conversion to
            // HashMap later handle de-duplication, so we just need to make sure that the entries
            // from the TOML are before the entries from the commandline.
            if let (Some(args_backends), Some(toml_backends)) =
                (&mut args.backends, &mut toml_tables.backends)
            {
                // when both are Some(), combined.backends should have the backends from args
                let combined_backends = combined.backends.as_mut().unwrap();
                assert_eq!(combined_backends, args_backends);
                // since there is no prepend(), get them in the right order first
                toml_backends.append(combined_backends);
                // then move them where we need them
                combined_backends.append(toml_backends);
            }
            // Exact same logic for dictionaries as for backends
            if let (Some(args_dicts), Some(toml_dicts)) =
                (&mut args.dictionaries, &mut toml_tables.dictionaries)
            {
                let combined_dicts = combined.dictionaries.as_mut().unwrap();
                assert_eq!(combined_dicts, args_dicts);
                toml_dicts.append(combined_dicts);
                combined_dicts.append(toml_dicts);
            }
            args = combined;
        }
        args
    }
}

fn parse_key_value<T, U>(s: &str) -> Result<(T, U), Box<dyn StdError>>
where
    T: FromStr,
    T::Err: StdError + 'static,
    U: FromStr,
    U::Err: StdError + 'static,
{
    let pos = s
        .find(':')
        .ok_or_else(|| format!("invalid KEY:value: no `:` found in `{}`", s))?;
    Ok((s[..pos].parse()?, s[pos + 1..].parse()?))
}

fn parse_backend(s: &str) -> Result<Backend, Box<dyn StdError>> {
    let (name, address) = parse_key_value(s)?;
    Ok(Backend { name, address })
}

fn parse_dictionary(s: &str) -> Result<Dictionary, Box<dyn StdError>> {
    let (name, v) = parse_key_value::<String, String>(s)?;
    let dict: Result<HashMap<String, String>, Box<dyn StdError>> =
        v.split(',').try_fold(HashMap::default(), |mut res, el| {
            let pos = el
                .find('=')
                .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{}`", el))?;
            res.insert(el[..pos].parse()?, el[pos + 1..].parse()?);
            Ok(res)
        });
    Ok(Dictionary {
        name,
        entries: dict?,
    })
}
