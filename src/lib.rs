use std::error::Error;

pub mod data;
pub mod moodle;
#[allow(dead_code, unused_variables)] // As of now, `download` contains unused legacy functions possibly needed in the future for proper TUM Live integration
pub mod download;
pub mod tum_live;
pub mod postprocessing;
pub mod http_headers;

/* TODOs
- parse live.rgb.tum lecture page, extract m3u8 urls
- (parse general live.rgb.tum page to display available lectures)
*/

pub type GenericError = Box<dyn Error + Send + Sync + 'static>;
pub type GenericResult<T> = Result<T, GenericError>;
