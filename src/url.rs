use crate::{event_handlers::Handles, statistics::StatError::UrlFormat, Command::AddError};
use anyhow::{anyhow, bail, Result};
use reqwest::Url;
use std::{convert::TryInto, fmt, sync::Arc};

/// abstraction around target urls; collects all Url related shenanigans in one place
#[derive(Debug)]
pub struct FeroxUrl {
    /// string representation of the target url
    target: String,

    /// Handles object for grabbing config values
    handles: Arc<Handles>,
}

/// implementation of FeroxUrl
impl FeroxUrl {
    /// Create new FeroxUrl given a target url as a string
    pub fn from_string(target: &str, handles: Arc<Handles>) -> Self {
        Self {
            handles,
            target: String::from(target),
        }
    }

    /// Create new FeroxUrl given a target url as a reqwest::Url
    pub fn from_url(target: &Url, handles: Arc<Handles>) -> Self {
        Self {
            handles,
            target: target.as_str().to_string(),
        }
    }

    /// Creates a vector of formatted Urls
    ///
    /// At least one value will be returned (base_url + word)
    ///
    /// If any extensions were passed to the program, each extension will add a
    /// (base_url + word + ext) Url to the vector
    pub fn formatted_urls(&self, word: &str) -> Result<Vec<Url>> {
        log::trace!("enter: formatted_urls({})", word);

        let mut urls = vec![];

        match self.format(word, None) {
            // default request, i.e. no extension
            Ok(url) => urls.push(url),
            Err(_) => self.handles.stats.send(AddError(UrlFormat))?,
        }

        for ext in self.handles.config.extensions.iter() {
            match self.format(word, Some(ext)) {
                // any extensions passed in
                Ok(url) => urls.push(url),
                Err(_) => self.handles.stats.send(AddError(UrlFormat))?,
            }
        }

        log::trace!("exit: formatted_urls -> {:?}", urls);
        Ok(urls)
    }

    /// Simple helper to generate a `Url`
    ///
    /// Errors during parsing `url` or joining `word` are propagated up the call stack
    pub fn format(&self, word: &str, extension: Option<&str>) -> Result<Url> {
        log::trace!("enter: format({}, {:?})", word, extension);

        if Url::parse(word).is_ok() {
            // when a full url is passed in as a word to be joined to a base url using
            // reqwest::Url::join, the result is that the word (url) completely overwrites the base
            // url, potentially resulting in requests to places that aren't actually the target
            // specified.
            //
            // in order to resolve the issue, we check if the word from the wordlist is a parsable URL
            // and if so, don't do any further processing
            let message = format!("word ({}) from wordlist is a URL, skipping...", word);
            log::warn!("{}", message);
            log::trace!("exit: format -> Err({})", message);
            bail!(message);
        }

        // from reqwest::Url::join
        //   Note: a trailing slash is significant. Without it, the last path component
        //   is considered to be a “file” name to be removed to get at the “directory”
        //   that is used as the base
        //
        // the transforms that occur here will need to keep this in mind, i.e. add a slash to preserve
        // the current directory sent as part of the url
        let url = if word.is_empty() {
            // v1.0.6: added during --extract-links feature implementation to support creating urls
            // that were extracted from response bodies, i.e. http://localhost/some/path/js/main.js
            self.target.to_string()
        } else if !self.target.ends_with('/') {
            format!("{}/", self.target)
        } else {
            self.target.to_string()
        };

        // extensions and slashes are mutually exclusive cases
        let word = if extension.is_some() {
            format!("{}.{}", word, extension.unwrap())
        } else if self.handles.config.add_slash && !word.ends_with('/') {
            // -f used, and word doesn't already end with a /
            format!("{}/", word)
        } else if word.starts_with("//") {
            // bug ID'd by @Sicks3c, when a wordlist contains words that begin with 2 forward slashes
            // i.e. //1_40_0/static/js, it gets joined onto the base url in a surprising way
            // ex: https://localhost/ + //1_40_0/static/js -> https://1_40_0/static/js
            // this is due to the fact that //... is a valid url. The fix is introduced here in 1.12.2
            // and simply removes prefixed forward slashes if there are two of them. Additionally,
            // trim_start_matches will trim the pattern until it's gone, so even if there are more than
            // 2 /'s, they'll still be trimmed
            word.trim_start_matches('/').to_string()
        } else {
            String::from(word)
        };

        let base_url = Url::parse(&url)?;
        let joined = base_url.join(&word)?;

        if self.handles.config.queries.is_empty() {
            // no query params to process
            log::trace!("exit: format -> {}", joined);
            Ok(joined)
        } else {
            let with_params =
                Url::parse_with_params(joined.as_str(), &self.handles.config.queries)?;
            log::trace!("exit: format_url -> {}", with_params);
            Ok(with_params) // request with params attached
        }
    }

    /// Gets the length of a url's path
    pub fn path_length(&self) -> Result<u64> {
        let parsed = Url::parse(&self.target)?;
        Ok(FeroxUrl::path_length_of_url(&parsed))
    }

    /// Gets the length of a url's path
    ///
    /// example: http://localhost/stuff -> 5
    pub fn path_length_of_url(url: &Url) -> u64 {
        log::trace!("enter: get_path_length({})", url);

        let path = url.path();

        let segments = if let Some(split) = path.strip_prefix('/') {
            split.split_terminator('/')
        } else {
            log::trace!("exit: get_path_length -> 0");
            return 0;
        };

        if let Some(last) = segments.last() {
            // failure on conversion should be very unlikely. While a usize can absolutely overflow a
            // u64, the generally accepted maximum for the length of a url is ~2000.  so the value we're
            // putting into the u64 should never realistically be anywhere close to producing an
            // overflow.
            // usize max: 18,446,744,073,709,551,615
            // u64 max:   9,223,372,036,854,775,807
            let url_len: u64 = last
                .len()
                .try_into()
                .expect("Failed usize -> u64 conversion");

            log::trace!("exit: get_path_length -> {}", url_len);
            return url_len;
        }

        log::trace!("exit: get_path_length -> 0");
        0
    }

    /// Simple helper to abstract away adding a forward-slash to a url if not present
    ///
    /// used mostly for deduplication purposes and url state tracking
    pub fn normalize(&self) -> String {
        log::trace!("enter: normalize");

        let normalized = if self.target.ends_with('/') {
            self.target.to_string()
        } else {
            format!("{}/", self.target)
        };

        log::trace!("exit: normalize -> {}", normalized);
        normalized
    }

    /// Helper function that determines the current depth of a given url
    ///
    /// Essentially looks at the Url path and determines how many directories are present in the
    /// given Url
    ///
    /// http://localhost -> 1
    /// http://localhost/ -> 1
    /// http://localhost/stuff -> 2
    /// ...
    ///
    /// returns 0 on error and relative urls
    pub fn depth(&self) -> Result<usize> {
        log::trace!("enter: get_depth");

        let target = self.normalize();

        let parsed = Url::parse(&target)?;
        let parts = parsed
            .path_segments()
            .ok_or_else(|| anyhow!("No path segments found"))?;

        // at least an empty string returned by the Split, meaning top-level urls
        let mut depth = 0;

        for _ in parts {
            depth += 1;
        }

        log::trace!("exit: get_depth -> {}", depth);
        Ok(depth)
    }
}

/// Display implementation for a FeroxUrl
impl fmt::Display for FeroxUrl {
    /// formatter for FeroxUrl
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", &self.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Configuration;

    #[test]
    /// sending url + word without any extensions should get back one url with the joined word
    fn formatted_urls_no_extension_returns_base_url_with_word() {
        let handles = Arc::new(Handles::for_testing(None, None).0);
        let url = FeroxUrl::from_string("http://localhost", handles);
        let urls = url.formatted_urls("turbo").unwrap();
        assert_eq!(urls, [Url::parse("http://localhost/turbo").unwrap()])
    }

    #[test]
    /// sending url + word + 1 extension should get back two urls, one base and one with extension
    fn formatted_urls_one_extension_returns_two_urls() {
        let config = Configuration {
            extensions: vec![String::from("js")],
            ..Default::default()
        };

        let handles = Arc::new(Handles::for_testing(None, Some(Arc::new(config))).0);
        let url = FeroxUrl::from_string("http://localhost", handles);
        let urls = url.formatted_urls("turbo").unwrap();

        assert_eq!(
            urls,
            [
                Url::parse("http://localhost/turbo").unwrap(),
                Url::parse("http://localhost/turbo.js").unwrap()
            ]
        )
    }

    #[test]
    /// sending url + word + multiple extensions should get back n+1 urls
    fn formatted_urls_multiple_extensions_returns_n_plus_one_urls() {
        let ext_vec = vec![
            vec![String::from("js")],
            vec![String::from("js"), String::from("php")],
            vec![String::from("js"), String::from("php"), String::from("pdf")],
            vec![
                String::from("js"),
                String::from("php"),
                String::from("pdf"),
                String::from("tar.gz"),
            ],
        ];
        let base = Url::parse("http://localhost/turbo").unwrap();
        let js = Url::parse("http://localhost/turbo.js").unwrap();
        let php = Url::parse("http://localhost/turbo.php").unwrap();
        let pdf = Url::parse("http://localhost/turbo.pdf").unwrap();
        let tar = Url::parse("http://localhost/turbo.tar.gz").unwrap();

        let expected = vec![
            vec![base.clone(), js.clone()],
            vec![base.clone(), js.clone(), php.clone()],
            vec![base.clone(), js.clone(), php.clone(), pdf.clone()],
            vec![base, js, php, pdf, tar],
        ];

        for (i, ext_set) in ext_vec.into_iter().enumerate() {
            let config = Configuration {
                extensions: ext_set,
                ..Default::default()
            };

            let handles = Arc::new(Handles::for_testing(None, Some(Arc::new(config))).0);
            let url = FeroxUrl::from_string("http://localhost", handles);

            let urls = url.formatted_urls("turbo").unwrap();
            assert_eq!(urls, expected[i]);
        }
    }

    #[test]
    /// base url returns 1
    fn depth_base_url_returns_1() {
        let handles = Arc::new(Handles::for_testing(None, None).0);
        let url = FeroxUrl::from_string("http://localhost", handles);

        let depth = url.depth().unwrap();
        assert_eq!(depth, 1);
    }

    #[test]
    /// base url with slash returns 1
    fn depth_base_url_with_slash_returns_1() {
        let handles = Arc::new(Handles::for_testing(None, None).0);
        let url = FeroxUrl::from_string("http://localhost/", handles);

        let depth = url.depth().unwrap();
        assert_eq!(depth, 1);
    }

    #[test]
    /// base url + 1 dir returns 2
    fn depth_one_dir_returns_2() {
        let handles = Arc::new(Handles::for_testing(None, None).0);
        let url = FeroxUrl::from_string("http://localhost/src", handles);

        let depth = url.depth().unwrap();
        assert_eq!(depth, 2);
    }

    #[test]
    /// base url + 1 dir and slash returns 2
    fn depth_one_dir_with_slash_returns_2() {
        let handles = Arc::new(Handles::for_testing(None, None).0);
        let url = FeroxUrl::from_string("http://localhost/src/", handles);

        let depth = url.depth().unwrap();
        assert_eq!(depth, 2);
    }

    #[test]
    /// base url + 1 word + no slash + no extension
    fn format_url_normal() {
        let handles = Arc::new(Handles::for_testing(None, None).0);
        let url = FeroxUrl::from_string("http://localhost", handles);
        let formatted = url.format("stuff", None).unwrap();

        assert_eq!(
            formatted,
            reqwest::Url::parse("http://localhost/stuff").unwrap()
        );
    }

    #[test]
    /// base url + no word + no slash + no extension
    fn format_url_no_word() {
        let handles = Arc::new(Handles::for_testing(None, None).0);
        let url = FeroxUrl::from_string("http://localhost", handles);
        let formatted = url.format("", None).unwrap();
        assert_eq!(formatted, reqwest::Url::parse("http://localhost").unwrap());
    }

    #[test]
    /// base url + word + no slash + no extension + queries
    fn format_url_joins_queries() {
        let config = Configuration {
            queries: vec![(String::from("stuff"), String::from("things"))],
            ..Default::default()
        };

        let handles = Arc::new(Handles::for_testing(None, Some(Arc::new(config))).0);
        let url = FeroxUrl::from_string("http://localhost", handles);
        let formatted = url.format("lazer", None).unwrap();

        assert_eq!(
            formatted,
            reqwest::Url::parse("http://localhost/lazer?stuff=things").unwrap()
        );
    }

    #[test]
    /// base url + no word + no slash + no extension + queries
    fn format_url_without_word_joins_queries() {
        let config = Configuration {
            queries: vec![(String::from("stuff"), String::from("things"))],
            ..Default::default()
        };

        let handles = Arc::new(Handles::for_testing(None, Some(Arc::new(config))).0);
        let url = FeroxUrl::from_string("http://localhost", handles);
        let formatted = url.format("", None).unwrap();

        assert_eq!(
            formatted,
            reqwest::Url::parse("http://localhost/?stuff=things").unwrap()
        );
    }

    #[test]
    #[should_panic]
    /// no base url is an error
    fn format_url_no_url() {
        let handles = Arc::new(Handles::for_testing(None, None).0);
        let url = FeroxUrl::from_string("", handles);
        url.format("stuff", None).unwrap();
    }

    #[test]
    /// word prepended with slash is adjusted for correctness
    fn format_url_word_with_preslash() {
        let handles = Arc::new(Handles::for_testing(None, None).0);
        let url = FeroxUrl::from_string("http://localhost", handles);
        let formatted = url.format("/stuff", None).unwrap();

        assert_eq!(
            formatted,
            reqwest::Url::parse("http://localhost/stuff").unwrap()
        );
    }

    #[test]
    /// word with appended slash allows the slash to persist
    fn format_url_word_with_postslash() {
        let handles = Arc::new(Handles::for_testing(None, None).0);
        let url = FeroxUrl::from_string("http://localhost", handles);
        let formatted = url.format("stuff/", None).unwrap();

        assert_eq!(
            formatted,
            reqwest::Url::parse("http://localhost/stuff/").unwrap()
        );
    }

    #[test]
    /// word with two prepended slashes doesn't discard the entire domain
    fn format_url_word_with_two_prepended_slashes() {
        let handles = Arc::new(Handles::for_testing(None, None).0);
        let url = FeroxUrl::from_string("http://localhost", handles);
        let formatted = url.format("//upload/img", None).unwrap();

        assert_eq!(
            formatted,
            reqwest::Url::parse("http://localhost/upload/img").unwrap()
        );
    }

    #[test]
    /// word that is a fully formed url, should return an error
    fn format_url_word_that_is_a_url() {
        let handles = Arc::new(Handles::for_testing(None, None).0);
        let url = FeroxUrl::from_string("http://localhost", handles);
        let formatted = url.format("http://schmocalhost", None);

        assert!(formatted.is_err());
    }
}
