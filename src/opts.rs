use std::{collections::HashMap, error::Error as StdError, path::PathBuf, str::FromStr};
use structopt::StructOpt;

/// ⏱️  A local Fastly Compute@Edge runtime emulator
#[derive(Debug, StructOpt)]
pub struct Opts {
    /// Path to a Fastly Compute@Edge .wasm file
    #[structopt(long, short, default_value = "bin/main.wasm")]
    pub(crate) wasm: PathBuf,
    /// Port to listen on
    #[structopt(long, short, default_value = "3000")]
    pub(crate) port: u16,
    /// Backend to proxy in backend-name:host format (foo:foo.org)
    #[structopt(long, short, parse(try_from_str = parse_key_value))]
    pub(crate) backend: Vec<(String, String)>,
    /// Edge dictionary in dictionary-name:key=value,key=value format
    #[structopt(long, short, parse(try_from_str = parse_dictionary))]
    pub(crate) dictionary: Vec<(String, HashMap<String, String>)>,
    #[structopt(long)]
    pub(crate) tls_cert: Option<PathBuf>,
    #[structopt(long)]
    pub(crate) tls_key: Option<PathBuf>,
    /// Watch for changes to .wasm file, reloading application when relevant
    #[structopt(long)]
    pub(crate) watch: bool,
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

fn parse_dictionary(s: &str) -> Result<(String, HashMap<String, String>), Box<dyn StdError>> {
    let (name, v) = parse_key_value::<String, String>(s)?;
    let dict: Result<HashMap<String, String>, Box<dyn StdError>> =
        v.split(',').try_fold(HashMap::default(), |mut res, el| {
            let pos = el
                .find('=')
                .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{}`", el))?;
            res.insert(el[..pos].parse()?, el[pos + 1..].parse()?);
            Ok(res)
        });
    Ok((name, dict?))
}
