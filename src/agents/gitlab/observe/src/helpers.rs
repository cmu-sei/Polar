pub mod helpers {
    use std::env;
    use url::Url;
    //TODO: this is a duplicate of the helpers in the consumer...create a lib if needed?
    pub fn get_rabbit_endpoint() -> String {
        let endpoint = env::var("RABBITMQ_ENDPOINT").expect("Could not load rabbitmq instance endpoint from enviornment.");
        match Url::parse(endpoint.as_str()) {

           Ok(url) => return url.to_string(),

           Err(e) => panic!("error: {}, the  provided rabbitmq endpoint is not valid.", e)
        }
    }

}