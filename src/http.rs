use fastly_shared::HttpVersion;
use hyper::Version;

pub fn version(version: Version) -> HttpVersion {
    match version {
        Version::HTTP_09 => HttpVersion::Http09,
        Version::HTTP_10 => HttpVersion::Http10,
        Version::HTTP_11 => HttpVersion::Http11,
        Version::HTTP_2 => HttpVersion::H2,
        Version::HTTP_3 => HttpVersion::H3,
        _ => HttpVersion::Http11,
    }
}
