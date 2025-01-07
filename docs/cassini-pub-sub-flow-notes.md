## Subscription flow

client connects, listener created, 

client registers

session is created

client sends request to subscribe to session

session forwards to broker
broker performs auth
if auth successful - broker looks for existing subscriber actor.
    If no subscriber exists, forward subscribe request topic IF it exists
        IF NOT, we should tell the topicmgr to create one, AND THEN subscribe by appending the subscriber_id ("session_id:topic") to the topic agent's list. (Consider abuse case of someone sending a bunch of publishes with random topic names)
   
        Forward subscribe request to sub mgr letting
        sub mgr creates subscriber actor
        new subscriber actor sends ACK directly to session with success

    IF subscriber exists, send ACK back to session with success


    session sends ACK to client via the listener IF it exists
    
    if not, log error and wait around to die OR for the listener to come back

ELSE, send 403 error


NOTE: if a subscriber *fails* i.e. panics, errors out, etc. Inform client to resend subscriptions
Our only other recourse is to just kill the session entirely...not ideal



## Publishing Flow

client connects
listener manager creates new listener
listener waits for registration request
client sends registration request
listener forwards registration request to broker
broker performs some auth
if auth successful,
    session is created
    session sends ACK to client via listener
    client sends request to subscribe to session

    session forwards to broker
    broker performs auth
        if auth successful - broker looks for existing topic actor.

            IF topic exists, 
                sends publish request to topic
                If any, topic actor forwards notifications to subscribers
                subscriber actor will call the session with a message containing
                the payload
                session will reply, unblocking the sub
                if session doesn't reply in time, it's probably dead
                    log the error, stop subscriber
                if session replies saying it failed to publish
                    add to message dead letter queue


            If no topic exists
                expect to call topic manager to add a topic, await it's reponse\

                if topicmgr succeeds in creating a new topic
                    if broker succeeds forwarding a publish request to topic 
                        broker sends PublishAck back to session that made the request
                    else
                        send error back to session

        else, 
            send 403 error
else,
    listener gets 403 type message



NOTE: We can use RPC to wait for a reply from subscribers and block while we wait for a reply,
If we timeout on any particular send, we can put the message back onto the queue 

NOTE: if a subscriber *fails* i.e. panics, errors out, etc. Inform client to resend subscriptions
Our only other recourse is to just kill the session entirely...not ideal


