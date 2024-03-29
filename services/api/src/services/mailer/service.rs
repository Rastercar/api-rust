use super::templates::{ConfirmEmailReplacements, RecoverPasswordReplacements};
use crate::{config::app_config, rabbitmq::Rmq};
use anyhow::Result;
use lapin::{
    options::BasicPublishOptions, publisher_confirm::PublisherConfirm, types::FieldTable,
    BasicProperties,
};
use shared::dto::mailer::{EmailRecipient, SendEmailIn};
use std::fs;
use std::sync::Arc;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use url;

pub enum ConfirmEmailRecipientType {
    User,
    Organization,
}

/// A abstraction to make RPC calls to the mailer microservice
#[derive(Clone)]
pub struct MailerService {
    rmq: Arc<Rmq>,
}

impl MailerService {
    pub fn new(rmq: Arc<Rmq>) -> MailerService {
        MailerService { rmq }
    }

    #[tracing::instrument(skip(self, payload))]
    async fn publish_to_mailer_service(
        &self,
        payload: &[u8],
        rpc_name: &str,
    ) -> Result<PublisherConfirm> {
        let span = Span::current();
        let ctx = span.context();

        let amqp_headers = shared::tracer::create_amqp_headers_with_span_ctx(&ctx);

        Ok(self
            .rmq
            .publish(
                shared::constants::rabbitmq::DEFAULT_EXCHANGE,
                shared::constants::rabbitmq::MAILER_QUEUE,
                BasicPublishOptions::default(),
                payload,
                BasicProperties::default()
                    .with_content_type("application/json".into())
                    .with_kind(rpc_name.into())
                    .with_headers(FieldTable::from(amqp_headers)),
            )
            .await?)
    }

    #[tracing::instrument(skip_all)]
    pub async fn send_email(&self, input: SendEmailIn) -> Result<PublisherConfirm> {
        self.publish_to_mailer_service(
            serde_json::to_string(&input)?.as_bytes(),
            shared::constants::rabbitmq::OP_SEND_EMAIL,
        )
        .await
    }

    #[tracing::instrument(skip(self, reset_password_token))]
    pub async fn send_recover_password_email(
        &self,
        email: String,
        reset_password_token: String,
        username: String,
    ) -> Result<PublisherConfirm> {
        let mut link = create_frontend_link("auth/change-password")?;
        link.set_query(Some(format!("token={}", reset_password_token).as_str()));

        let replacements = Some(Into::into(RecoverPasswordReplacements {
            username,
            reset_password_link: link.into(),
        }));

        let html = &read_template("recover-password")?;

        let email = SendEmailIn::default()
            .with_subject("Rastercar: recover password")
            .with_body_html(html)
            .with_to(vec![EmailRecipient {
                email,
                replacements,
            }]);

        self.send_email(email).await
    }

    #[tracing::instrument(skip(self, reset_password_token, recipient_type))]
    pub async fn send_confirm_email_address_email(
        &self,
        email: String,
        reset_password_token: String,
        recipient_type: ConfirmEmailRecipientType,
    ) -> Result<PublisherConfirm> {
        let mut link = create_frontend_link("auth/confirm-email-address")?;

        let (query, title) = match recipient_type {
            ConfirmEmailRecipientType::User => (
                format!("token={}", reset_password_token),
                String::from("Thanks for registering a Rastercar account"),
            ),

            ConfirmEmailRecipientType::Organization => (
                format!("token={}&confirmingFor=organization", reset_password_token),
                String::from("Thanks for creating your rastercar organization"),
            ),
        };

        link.set_query(Some(&query));

        let replacements = Some(Into::into(ConfirmEmailReplacements {
            title,
            confirmation_link: link.into(),
        }));

        let email = SendEmailIn::default()
            .with_subject("Rastercar: confirm email")
            .with_body_html(&read_template("confirm-email")?)
            .with_to(vec![EmailRecipient {
                email,
                replacements,
            }]);

        self.send_email(email).await
    }
}

/// creates a link to the rastercar frontend
fn create_frontend_link(path: &str) -> Result<url::Url, url::ParseError> {
    app_config().frontend_url.join(path)
}

/// creates a link to the rastercar frontend
fn read_template(template: &str) -> std::io::Result<String> {
    fs::read_to_string(format!("templates/{}.hbs", template))
}
