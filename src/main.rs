use std::env;
use std::fs;
use std::collections::HashMap;
use std::ops::Index;
use std::sync::Mutex;

use dotenv;
use json;
use url::Url;

use serenity::async_trait;
use serenity::model::channel::{Channel,Message};
use serenity::model::gateway::Ready;
use serenity::model::webhook::Webhook;
use serenity::model::user::User;
use serenity::model::channel::AttachmentType;
use serenity::model::event::MessageUpdateEvent;
use serenity::model::id::{MessageId,ChannelId,GuildId};
use serenity::prelude::*;



#[derive(Default)]
struct Bot {
	// Key: ID of the channel.  Value: ID of channels in which to repeat message.
	direct_messages_to : HashMap<u64, Vec<u64>>,
	
	// Key: ID of the message.  Value: (ID of message, ID of channel) which have been repeated.
	message_cache : Mutex<HashMap<u64, Vec<(u64, u64)>>>,
	
	// Key: ID of the message.  Value: ID of author.
	message_author_cache : Mutex<HashMap<u64, u64>>,
	
	// Key: (ID of channel, ID of user).  Value: ID of the webhook.
	webhook_cache : Mutex<HashMap<(u64,u64),u64>>,
	
	// Silly message counter for debug purposes.
	message_counter : Mutex<i32>,
}


impl Bot {
	fn load(&mut self) {
		self.load_config();
	}
	
	fn load_config(&mut self) {
		// Initialize JSON.
		let json_file = fs::read_to_string("config.json").expect("Please create the config.json file.");
		let json_object = json::parse(&json_file[..]).expect("Please insert a valid JSON in config.json");
		let json_array = match json_object {
			json::JsonValue::Array(arr) => arr,
			_ => panic!("An array in the config.json was expected."),
		};
		
		// Initialize direct map.
		// let mut direct_map : HashMap<u64,Vec<u64>> = HashMap::new();
		for item in json_array.iter() {
			let key_value_array = match item {
				json::JsonValue::Array(arr) => arr,
				_ => panic!("An array in the config.json was expected."),
			};
			
			let mut iterator = key_value_array.iter();
			let key = iterator.next().expect("Array cannot be empty").as_u64().expect("Positive integers were expected");
			let value = iterator.map(|id| id.as_u64().expect("Positive integers were expected")).collect();		
			
			self.direct_messages_to.insert(key, value);
		}
	}
}



impl Index<u64> for Bot {
	type Output = [u64];
    fn index<'a>(&'a self, key : u64) -> &'a [u64] {
        match self.direct_messages_to.get(&key) {
			Some(arr) => &arr[..],
			None => &[],
		}
    }
}




impl Bot {
	async fn ping_pong(&self, ctx: &Context, msg: &Message) {
		if msg.content == "ping" {
			println!("Channel ID: {}", msg.channel_id);
			
			if let Err(why) = msg.channel_id.say(&ctx.http, "Pong!").await {
				println!("Error sending message: {:?}", why);
			}
		}
	}
	
	
	// Get a copy of the message cache.
	async fn get_message_cache(&self, message_id : u64) -> Option<Vec<(u64, u64)>> {
		let msg_cache_mutex = self.message_cache.lock().unwrap();
		if msg_cache_mutex.contains_key(&message_id) == true {
			let mut value : Vec<(u64, u64)> = Vec::new();
			for (repeated_message_id, repeated_channel_id) in msg_cache_mutex[&message_id].iter() {
				value.push((*repeated_message_id, *repeated_channel_id));
			}
			
			Some(value)
		} else {
			None
		}
	}
	
	
	// Get a copy of the webhook cache.
	async fn get_webhook_cache(&self, channel_id : u64, author_id : u64) -> Option<u64> {
		let webhook_cache = self.webhook_cache.lock().unwrap();
		let pair = (channel_id, author_id);
		match webhook_cache.get(&pair) {
			Some(id) => Some(*id),
			None => None,
		}
	}
	
	
	// Get a copy of the message author cache.
	async fn get_message_author_cache(&self, message_id : u64) -> Option<u64> {
		let msg_author_cache = self.message_author_cache.lock().unwrap();
		match msg_author_cache.get(&message_id) {
			Some(id) => Some(*id),
			None => None,
		}
	}
	
	
	// Create a webhook for a user.
	async fn create_user_webhook(&self, ctx: &Context, msg: &Message, _ : &User, target_channel : &Channel) -> Result<Webhook, SerenityError> {
		// Create the webhook.
		let webhook_result = match msg.author.avatar_url() {
			Some(avatar_url) => target_channel.id().create_webhook_with_avatar(
				&ctx.http,
				msg.author.name.to_string(),
				AttachmentType::Image(Url::parse(avatar_url.as_str()).unwrap()),
			).await,
			
			None => target_channel.id().create_webhook(
				&ctx.http,
				msg.author.name.to_string(),
			).await,
		};
		
		// Save in the cache.
		if let Ok(webhook_object) = &webhook_result {
			let mut webhook_cache = self.webhook_cache.lock().unwrap();
			let pair = (*msg.channel_id.as_u64(), *msg.author.id.as_u64());
			webhook_cache.insert(pair, *webhook_object.id.as_u64());
		}
		
		return webhook_result;
	}
	
	
	// Gets a user webhook. If it doesn't exist, creates one.
	async fn get_user_webhook(&self, ctx: &Context, msg: &Message, user: &User, target_channel : &Channel) -> Result<Webhook, SerenityError> {
		// Get webhook ID belonging to the user.
		let pair = (*msg.channel_id.as_u64(), *msg.author.id.as_u64());
		let webhook_user_id: Option<u64> = match self.webhook_cache.lock().unwrap().get(&pair) {
			Some(id) => Some(*id),
			None => None,
		};
		
		//let webhook_user_id : Option<&u64> = self.webhook_cache.lock().unwrap().get(&pair);
		
		// Found, search for it.
		if let Some(whuid) = webhook_user_id {
			return match ctx.http.get_webhook(whuid).await {
				Ok(webhook) => Ok(webhook),
				Err(_) => self.create_user_webhook(ctx, msg, user, target_channel).await,
			}
		}
		
		else {
			return self.create_user_webhook(ctx, msg, user, target_channel).await;
		}
	}
}


#[async_trait]
impl EventHandler for Bot {
	async fn message_update(&self, ctx: Context, _old_msg : Option<Message>, _new_msg : Option<Message>, update_event : MessageUpdateEvent) {
		// Get data from the message.
		let channel_id = *update_event.channel_id.as_u64();
		let message_id = *update_event.id.as_u64();
		let author_id = match update_event.author {
			Some(author) => *author.id.as_u64(),
			None => *ctx.http.get_message(*update_event.channel_id.as_u64(), *update_event.id.as_u64()).await.unwrap().author.id.as_u64(),
		};
		
		let message_content = match update_event.content {
			Some(content) => content,
			None => { return; }
		};
		

		// Repeat the message edit.
		if let Some(cached_messages) = self.get_message_cache(message_id).await {
			for (repeated_message_id, _repeated_channel_id) in cached_messages {
				let webhook_id = match self.get_webhook_cache(channel_id, author_id).await {
					Some(id) => id,
					None => continue,
				};
				
				
				match Webhook::from_id(&ctx.http, webhook_id).await {
					Ok(webhook) => match webhook.edit_message(&ctx.http, MessageId::from(repeated_message_id), |m| m.content(&message_content)).await {
						Ok(_) => continue,
						Err(why) => println!("Couldn't edit message: {:?}", why),
					}
					Err(why) => println!("Couldn't get webhook from id: {:?}", why),
				}
			}
		}
	}
	
	async fn message_delete(&self, ctx: Context, channel_id : ChannelId, deleted_message_id : MessageId, _guild_id : Option<GuildId>) {
		if let Some(cached_messages) = self.get_message_cache(*deleted_message_id.as_u64()).await {
			let message_id = *deleted_message_id.as_u64();
			let author_id = self.get_message_author_cache(message_id).await.unwrap();
			
			for (repeated_message_id, _repeated_channel_id) in cached_messages {
				let webhook_id = match self.get_webhook_cache(*channel_id.as_u64(), author_id).await {
					Some(id) => id,
					None => continue,
				};
				
				match Webhook::from_id(&ctx.http, webhook_id).await {
					Ok(webhook) => match webhook.delete_message(&ctx.http, MessageId::from(repeated_message_id)).await {
						Ok(_) => continue,
						Err(why) => println!("Couldn't delete message: {:?}", why),
					}
					Err(why) => println!("Couldn't get webhook from id: {:?}", why),
				}
			}
		}
	}
	
	async fn message(&self, ctx: Context, msg: Message) {
		// Display message on screen.
		println!("[{}] [{}] {}", msg.channel_id, msg.author.name, msg.content);
		println!("message_cache: {:?}", self.message_cache);
		println!("webhook_cache: {:?}", self.webhook_cache);
		println!("counter: {:?}", self.message_counter);
		
		// Augument counter.
		{
			let mut count = self.message_counter.lock().unwrap();
			*count += 1;
		}
		
		// Ignore if message is from self, or from webhook.
		if msg.author.id == ctx.cache.current_user_id() { return;}
		if msg.webhook_id.is_some() { return; }
		
		
		// Ping pong.
		self.ping_pong(&ctx, &msg).await;
		
		
		// Verify if it is in a channel to be repeated.
		if self.direct_messages_to.contains_key(msg.channel_id.as_u64()) == false {
			return;
		}
		
		// Create message cache: (ID of message, ID of channel)
		let mut cache : Vec<(u64, u64)> = Vec::new();
		
		// Repeat message.
		for channel_id in self.direct_messages_to[msg.channel_id.as_u64()].iter() {
			let text_channel = ctx.http.get_channel(*channel_id).await.unwrap();
			let webhook_user = self.get_user_webhook(&ctx, &msg, &msg.author, &text_channel).await;

			// Finally, by all means, send the message.
			if let Ok(webhook_object) = webhook_user {
				match webhook_object.execute(&ctx.http, true, |w| {
					for attachment in msg.attachments.iter() {
						if attachment.height.is_some() {
							w.add_file(AttachmentType::Image(Url::parse(&attachment.url).unwrap()));
						}
					}
					w.content(&msg.content)
				}).await {
					Ok(response) => cache.push((*response.unwrap().id.as_u64(), *channel_id)),
					Err(why) => println!("Error sending message: {:?}", why),
				}
			}
			
			else if let Err(why) = webhook_user {
				println!("Webhook object displayed error: {:?}", why);
			}
		}
		
		// Save cache information.
		{
			let mut msg_cache = self.message_cache.lock().unwrap();
			msg_cache.insert(*msg.id.as_u64(), cache);
		}
		
		{
			let mut msg_author_cache = self.message_author_cache.lock().unwrap();
			msg_author_cache.insert(*msg.id.as_u64(), *msg.author.id.as_u64());
		}
	}
	
	
	async fn ready(&self, ctx: Context, ready: Ready) {
		println!("{} is connected!", ready.user.name);
		
		if let Ok(servers) = ready.user.guilds(&ctx.http).await {
			for (index, server) in servers.into_iter().enumerate() {
				println!("[{}] [{}] {}", index, server.id.as_u64(), server.name)
			}
		}
	}
}



#[tokio::main]
async fn main() {
	// Initialize environment variables.
	dotenv::dotenv().ok();
	
	// Load bot.
	let mut bot : Bot = Default::default();
	bot.load();
	
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment.");
	let intents = GatewayIntents::GUILD_MESSAGES
		| GatewayIntents::DIRECT_MESSAGES
		| GatewayIntents::MESSAGE_CONTENT;
	
	let mut client = Client::builder(&token, intents).event_handler(bot).await.expect("Err creatint client");
	
	if let Err(why) = client.start().await {
		println!("Client error: {:?}", why);
	}
}
