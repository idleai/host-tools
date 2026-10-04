//! Packaged history RPC with a fixed host-installed chain binding.

use std::{
    io::{self, Read, Write},
    path::PathBuf,
};

use editchain_engine::queries::ChainQueries;
use idle_history::binding::RepositoryChainBinding;
use serde::{Deserialize, Serialize};

use super::{Failure, FailureCode, Preview, Request, Source, prepare, validate_binding};

const MAX_REQUEST: usize = 8 * 1024 * 1024;
const MAX_RESPONSE: usize = 64 * 1024 * 1024;

/// Storage locations supplied by the trusted host, never by an action request.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Logical repository selection attached to both sources.
    pub repository: RepositoryChainBinding,
    /// Absolute current-chain directory on the file-owning host.
    pub chain_directory: PathBuf,
    /// Explicit retained source, including its own blobs if content is needed.
    pub retained_directory: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    id: u64,
    body: serde_json::Value,
}

#[derive(Serialize)]
struct Response {
    id: u64,
    body: ResponseBody,
}

// Serialize complete typed results directly; a JSON value per binary byte would
// multiply native memory use before the transport can enforce its response limit.
#[derive(Serialize)]
#[serde(untagged)]
enum ResponseBody {
    Query(Box<idle_history::query::QueryOutput>),
    Projection(Box<idle_history::projections::ProjectionOutput>),
    Native(Box<Result<Preview, Failure>>),
    Activity(Box<Result<crate::activity::Preview, Failure>>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivityRequest {
    activity: crate::activity::Request,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryRequest {
    binding: RepositoryChainBinding,
    query: idle_history::query::Query,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectionRequest {
    binding: RepositoryChainBinding,
    projection: idle_history::projections::ProjectionQuery,
    #[serde(default)]
    inputs: Option<Vec<idle_protocol::v1::projections::ProjectionInput>>,
}

fn execute_body(binding: &Binding, body: serde_json::Value) -> ResponseBody {
    if body.get("projection").is_some() {
        let result = serde_json::from_value::<ProjectionRequest>(body)
            .map_err(|_error| idle_history::query::Error {
                message: "Invalid projection query.".to_owned(),
            })
            .and_then(|request| read_projection(binding, &request));
        ResponseBody::Projection(Box::new(result))
    } else if body.get("activity").is_some() {
        let result = serde_json::from_value::<ActivityRequest>(body)
            .map_err(|_error| {
                Failure::new(FailureCode::InvalidReference, "Invalid activity request.")
            })
            .and_then(|request| read_activity(binding, &request.activity));
        ResponseBody::Activity(Box::new(result))
    } else if body.get("query").is_some() {
        let result = serde_json::from_value::<QueryRequest>(body)
            .map_err(|_error| idle_history::query::Error {
                message: "Invalid history query.".to_owned(),
            })
            .and_then(|request| read_query(binding, &request));
        ResponseBody::Query(Box::new(result))
    } else {
        let result = serde_json::from_value(body)
            .map_err(|_error| {
                Failure::new(
                    FailureCode::InvalidReference,
                    "Invalid native history request.",
                )
            })
            .and_then(|request| execute(binding, &request));
        ResponseBody::Native(Box::new(result))
    }
}

fn read_query(binding: &Binding, request: &QueryRequest) -> idle_history::query::QueryOutput {
    let failure = |message: &str| idle_history::query::Error {
        message: message.to_owned(),
    };
    if request.binding != binding.repository || request.query.chain != binding.repository.chain {
        return Err(failure(
            "The query belongs to a different repository or chain.",
        ));
    }
    if !binding.chain_directory.is_dir() {
        return Err(failure("The bound history source is unavailable."));
    }
    let mut queries = ChainQueries::open(&binding.chain_directory)
        .map_err(|_error| failure("Unable to read the bound history source."))?;
    crate::query::execute(&mut queries, &binding.repository.chain, &request.query)
}

fn read_projection(
    binding: &Binding,
    request: &ProjectionRequest,
) -> idle_history::projections::ProjectionOutput {
    let failure = |message: &str| idle_history::query::Error {
        message: message.to_owned(),
    };
    if request.binding != binding.repository
        || request.projection.context.workspace != binding.repository.workspace_id
        || request.projection.context.chain != binding.repository.chain
    {
        return Err(failure(
            "The projection belongs to a different repository or chain.",
        ));
    }
    if !binding.chain_directory.is_dir() {
        return Err(failure("The bound history source is unavailable."));
    }
    let mut queries = ChainQueries::open(&binding.chain_directory)
        .map_err(|_error| failure("Unable to read the bound history source."))?;
    crate::projections::execute(
        &mut queries,
        &binding.repository.chain,
        &request.projection,
        &RepositoryMapper {
            inputs: request.inputs.as_deref(),
        },
    )
}

struct RepositoryMapper<'a> {
    inputs: Option<&'a [idle_protocol::v1::projections::ProjectionInput]>,
}

impl crate::projections::ProjectionMapper for RepositoryMapper<'_> {
    fn inputs(
        &self,
        queries: &ChainQueries,
        read: &crate::projections::ProjectionRead,
    ) -> Result<Vec<idle_protocol::v1::projections::ProjectionInput>, idle_history::query::Error>
    {
        let Some(inputs) = self.inputs else {
            let mut inputs = crate::projections::UnavailableMapper.inputs(queries, read)?;
            for input in &mut inputs {
                for gap in &mut input.gaps {
                    gap.message = "The repository reader is unavailable. Refresh repository to retry; local Activity remains readable.".into();
                }
            }
            return Ok(inputs);
        };
        Ok(idle_repository::checked_inputs(queries, inputs))
    }
}

/// Serve length-prefixed JSON until the host closes standard input.
/// Index handles are released after each read so other engine consumers can
/// use the same derived checkpoint between native actions.
///
/// # Errors
/// Returns malformed framing, invalid installation or transport I/O errors.
pub fn serve(mut input: impl Read, mut output: impl Write, binding: &Binding) -> io::Result<()> {
    validate_binding(&binding.repository).map_err(io::Error::other)?;
    if !binding.chain_directory.is_absolute()
        || binding
            .retained_directory
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
    {
        return Err(io::Error::other(
            "history sources require absolute directories",
        ));
    }
    loop {
        let mut header = [0; 4];
        if input.read(
            header
                .get_mut(..1)
                .ok_or_else(|| io::Error::other("frame header"))?,
        )? == 0
        {
            return Ok(());
        }
        input.read_exact(
            header
                .get_mut(1..)
                .ok_or_else(|| io::Error::other("frame header"))?,
        )?;
        let length = usize::try_from(u32::from_le_bytes(header)).map_err(io::Error::other)?;
        if length == 0 || length > MAX_REQUEST {
            return Err(io::Error::other("invalid history request length"));
        }
        let mut bytes = vec![0; length];
        input.read_exact(&mut bytes)?;
        let request: Envelope = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let response = Response {
            id: request.id,
            body: execute_body(binding, request.body),
        };
        let mut bytes = serde_json::to_vec(&response).map_err(io::Error::other)?;
        if bytes.len() > MAX_RESPONSE {
            bytes = serde_json::to_vec(&Response {
                id: request.id,
                body: ResponseBody::Native(Box::new(Err(Failure::new(
                    FailureCode::TooLarge,
                    "The complete preview exceeds the native transport limit.",
                )))),
            })
            .map_err(io::Error::other)?;
        }
        output.write_all(
            &u32::try_from(bytes.len())
                .map_err(io::Error::other)?
                .to_le_bytes(),
        )?;
        output.write_all(&bytes)?;
        output.flush()?;
    }
}

fn execute(binding: &Binding, request: &Request) -> Result<Preview, Failure> {
    let directory = directory(binding, &request.binding, request.source)?;
    let mut queries = ChainQueries::open(directory)?;
    prepare(&mut queries, &binding.repository, request.source, request)
}

fn read_activity(
    binding: &Binding,
    request: &crate::activity::Request,
) -> Result<crate::activity::Preview, Failure> {
    let directory = directory(binding, &request.binding, request.source)?;
    let mut queries = ChainQueries::open(directory)?;
    crate::activity::prepare(&mut queries, &binding.repository, request.source, request)
}

fn directory<'a>(
    binding: &'a Binding,
    repository: &RepositoryChainBinding,
    source: Source,
) -> Result<&'a PathBuf, Failure> {
    if *repository != binding.repository {
        return Err(Failure::new(
            FailureCode::BindingMismatch,
            "The action belongs to a different repository or chain.",
        ));
    }
    let directory = match source {
        Source::Current => &binding.chain_directory,
        Source::Retained => binding.retained_directory.as_ref().ok_or_else(|| {
            Failure::new(
                FailureCode::Unavailable,
                "A retained input source has not been bound.",
            )
        })?,
    };
    if !directory.is_dir() {
        return Err(Failure::new(
            FailureCode::Unavailable,
            "The bound history source is unavailable.",
        ));
    }
    Ok(directory)
}
