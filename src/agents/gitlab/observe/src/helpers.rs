pub mod helpers {
    use std::process;

    use reqwest::Client;
    use log::error;
    
    pub fn web_client() -> Client {
        match Client::builder().build() {
            Ok(client) => client,
            Err(e) => {
                error!("Cannot create web client, {}", e);
                process::exit(1)
            }
        }
        
    }
}