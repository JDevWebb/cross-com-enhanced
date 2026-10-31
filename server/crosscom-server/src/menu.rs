//! What the game asks for while the main menu loads, from its generated protocols: title
//! storage content, the tracking configuration and the locale. For now they answer that there's
//! nothing; the server logs what the game asked for. (Friends are in `social`.)

use grfs_protocols::localization_service::localization_protocol::*;
use grfs_protocols::title_storage::title_storage_protocol::*;
use grfs_protocols::tracking_service::tracking_protocol_3::*;
use quazal::prudp::ClientRegistry;
use quazal::rmc::types::QList;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::ClientInfo;
use quazal::Context;
use slog::debug;
use slog::info;
use slog::Logger;

use crate::ticket::player;

struct TitleStorage;

impl<CI> TitleStorageProtocolServerTrait<CI> for TitleStorage {
    fn browse_contents_by_type(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: BrowseContentsByTypeRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<BrowseContentsByTypeResponse, Error> {
        player(ci)?;
        info!(logger, "title storage: {:?} asked for (none served yet)", request.content_type);
        Ok(BrowseContentsByTypeResponse { contents: QList(vec![]) })
    }
}

/// The game's tracking principal reports here; tags are accepted and dropped.
struct Tracking;

fn signed_in<CI>(ci: &ClientInfo<CI>) -> Result<u32, Error> {
    ci.user_id.ok_or(Error::AccessDenied)
}

impl<CI> TrackingProtocol3ServerTrait<CI> for Tracking {
    fn send_tag(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: SendTagRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<SendTagResponse, Error> {
        signed_in(ci)?;
        debug!(logger, "tracking tag {:?}", request.tag);
        Ok(SendTagResponse)
    }

    fn send_tag_and_update_user_info(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: SendTagAndUpdateUserInfoRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<SendTagAndUpdateUserInfoResponse, Error> {
        signed_in(ci)?;
        debug!(logger, "tracking tag {:?}", request.tag);
        Ok(SendTagAndUpdateUserInfoResponse)
    }

    fn get_configuration(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        _request: GetConfigurationRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetConfigurationResponse, Error> {
        signed_in(ci)?;
        Ok(GetConfigurationResponse { tags: vec![] })
    }

    fn send_tags(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: SendTagsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<SendTagsResponse, Error> {
        signed_in(ci)?;
        debug!(logger, "{} tracking tags", request.tag_data.len());
        Ok(SendTagsResponse)
    }
}

struct Localization;

impl<CI> LocalizationProtocolServerTrait<CI> for Localization {
    fn get_locale_code(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        _request: GetLocaleCodeRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetLocaleCodeResponse, Error> {
        player(ci)?;
        Ok(GetLocaleCodeResponse { local_code: "en-US".into() })
    }

    fn set_locale_code(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: SetLocaleCodeRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<SetLocaleCodeResponse, Error> {
        player(ci)?;
        debug!(logger, "locale {:?}", request.local_code);
        Ok(SetLocaleCodeResponse)
    }
}

pub fn protocols<CI: 'static>() -> Vec<Box<dyn Protocol<CI>>> {
    vec![
        Box::new(TitleStorageProtocolServer::new(TitleStorage)),
        Box::new(TrackingProtocol3Server::new(Tracking)),
        Box::new(LocalizationProtocolServer::new(Localization)),
    ]
}
