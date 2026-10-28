//! Registering on the secure server (`SecureConnectionProtocol`): the game sends its own station
//! URLs and gets its connection ID and its public address back.

use grfs_protocols::secure_connection_service::secure_connection_protocol::*;
use quazal::prudp::ClientRegistry;
use quazal::rmc::types::QResult;
use quazal::rmc::types::StationURL;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::ClientInfo;
use quazal::Context;
use slog::info;
use slog::Logger;

struct SecureConnection;

/// The connection ID and the address the server sees the player at.
fn registered<CI>(ci: &ClientInfo<CI>, sid: u8, ty: u8) -> Result<(u32, StationURL), Error> {
    ci.user_id.ok_or(Error::AccessDenied)?;
    let connection_id = ci.connection_id.ok_or(Error::InternalError)?.into();
    let url = format!("prudp:/address={};port={};sid={sid};type={ty}", ci.address().ip(), ci.address().port())
        .parse()
        .map_err(|_| Error::InternalError)?;
    Ok((connection_id, url))
}

impl<CI> SecureConnectionProtocolServerTrait<CI> for SecureConnection {
    fn register(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: RegisterRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<RegisterResponse, Error> {
        let (pid_connection_id, url_public) = registered(ci, 14, 2)?;
        info!(logger, "{:?} registered: {:?}", ci.user_id, request.vec_my_urls);
        Ok(RegisterResponse {
            return_value: QResult::Ok,
            pid_connection_id,
            url_public,
        })
    }

    fn register_ex(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: RegisterExRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<RegisterExResponse, Error> {
        let (pid_connection_id, url_public) = registered(ci, 15, 3)?;
        info!(logger, "{:?} registered: {:?}", ci.user_id, request.vec_my_urls);
        Ok(RegisterExResponse {
            return_value: QResult::Ok,
            pid_connection_id,
            url_public,
        })
    }
}

pub fn new_protocol<CI: 'static>() -> Box<dyn Protocol<CI>> {
    Box::new(SecureConnectionProtocolServer::new(SecureConnection))
}
