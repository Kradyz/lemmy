use crate::{
  activities::{
    block::{generate_cc, SiteOrCommunity},
    community::send_activity_in_community,
    generate_activity_id,
    send_lemmy_activity,
    verify_is_public,
  },
  activity_lists::AnnouncableActivities,
  local_instance,
  objects::{instance::remote_instance_inboxes, person::ApubPerson},
  protocol::activities::block::{block_user::BlockUser, undo_block_user::UndoBlockUser},
  ActorType,
};
use activitypub_federation::{
  core::object_id::ObjectId,
  data::Data,
  traits::{ActivityHandler, Actor},
  utils::verify_domains_match,
};
use activitystreams_kinds::{activity::UndoType, public};
use lemmy_api_common::context::LemmyContext;
use lemmy_db_schema::{
  source::{
    community::{CommunityPersonBan, CommunityPersonBanForm},
    moderator::{ModBan, ModBanForm, ModBanFromCommunity, ModBanFromCommunityForm},
    person::{Person, PersonUpdateForm},
  },
  traits::{Bannable, Crud},
};
use lemmy_utils::error::LemmyError;
use url::Url;

impl UndoBlockUser {
  #[tracing::instrument(skip_all)]
  pub async fn send(
    target: &SiteOrCommunity,
    user: &ApubPerson,
    mod_: &ApubPerson,
    reason: Option<String>,
    context: &LemmyContext,
  ) -> Result<(), LemmyError> {
    let block = BlockUser::new(target, user, mod_, None, reason, None, context).await?;
    let audience = if let SiteOrCommunity::Community(c) = target {
      Some(ObjectId::new(c.actor_id()))
    } else {
      None
    };

    let id = generate_activity_id(
      UndoType::Undo,
      &context.settings().get_protocol_and_hostname(),
    )?;
    let undo = UndoBlockUser {
      actor: ObjectId::new(mod_.actor_id()),
      to: vec![public()],
      object: block,
      cc: generate_cc(target, context.pool()).await?,
      kind: UndoType::Undo,
      id: id.clone(),
      audience,
    };

    let mut inboxes = vec![user.shared_inbox_or_inbox()];
    match target {
      SiteOrCommunity::Site(_) => {
        inboxes.append(&mut remote_instance_inboxes(context.pool()).await?);
        send_lemmy_activity(context, undo, mod_, inboxes, false).await
      }
      SiteOrCommunity::Community(c) => {
        let activity = AnnouncableActivities::UndoBlockUser(undo);
        send_activity_in_community(activity, mod_, c, inboxes, true, context).await
      }
    }
  }
}

#[async_trait::async_trait(?Send)]
impl ActivityHandler for UndoBlockUser {
  type DataType = LemmyContext;
  type Error = LemmyError;

  fn id(&self) -> &Url {
    &self.id
  }

  fn actor(&self) -> &Url {
    self.actor.inner()
  }

  #[tracing::instrument(skip_all)]
  async fn verify(
    &self,
    context: &Data<LemmyContext>,
    request_counter: &mut i32,
  ) -> Result<(), LemmyError> {
    verify_is_public(&self.to, &self.cc)?;
    verify_domains_match(self.actor.inner(), self.object.actor.inner())?;
    self.object.verify(context, request_counter).await?;
    Ok(())
  }

  #[tracing::instrument(skip_all)]
  async fn receive(
    self,
    context: &Data<LemmyContext>,
    request_counter: &mut i32,
  ) -> Result<(), LemmyError> {
    let instance = local_instance(context).await;
    let expires = self.object.expires.map(|u| u.naive_local());
    let mod_person = self
      .actor
      .dereference(context, instance, request_counter)
      .await?;
    let blocked_person = self
      .object
      .object
      .dereference(context, instance, request_counter)
      .await?;
    match self
      .object
      .target
      .dereference(context, instance, request_counter)
      .await?
    {
      SiteOrCommunity::Site(_site) => {
        let blocked_person = Person::update(
          context.pool(),
          blocked_person.id,
          &PersonUpdateForm::builder()
            .banned(Some(false))
            .ban_expires(Some(expires))
            .build(),
        )
        .await?;

        // write mod log
        let form = ModBanForm {
          mod_person_id: mod_person.id,
          other_person_id: blocked_person.id,
          reason: self.object.summary,
          banned: Some(false),
          expires,
        };
        ModBan::create(context.pool(), &form).await?;
      }
      SiteOrCommunity::Community(community) => {
        let community_user_ban_form = CommunityPersonBanForm {
          community_id: community.id,
          person_id: blocked_person.id,
          expires: None,
        };
        CommunityPersonBan::unban(context.pool(), &community_user_ban_form).await?;

        // write to mod log
        let form = ModBanFromCommunityForm {
          mod_person_id: mod_person.id,
          other_person_id: blocked_person.id,
          community_id: community.id,
          reason: self.object.summary,
          banned: Some(false),
          expires,
        };
        ModBanFromCommunity::create(context.pool(), &form).await?;
      }
    }

    Ok(())
  }
}
