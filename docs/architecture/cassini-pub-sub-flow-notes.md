## **Subscription Flow**
1. Client **connects**.
2. **ListenerManager** creates a new **Listener** actor.
3. **Client sends a registration request** to the Listener.
4. **Listener forwards registration request** to the **Broker**.
5. **Broker performs authentication**.
   - If **auth fails**, Broker sends a **403 error** back to the **Listener**, which forwards it to the **Client**.
   - If **auth succeeds**, proceed.
6. **Broker looks for an existing session**.
   - If no session exists:
     1. **Broker requests SessionManager to create a new session**.
     2. **SessionAgent is created and linked to the Client Listener**.
     3. **SessionAgent sends a registration ACK to the Client via the Listener**.
7. **Client sends a request to subscribe to a topic**.
8. **Listener forwards subscription request** to **SessionAgent**.
9. **SessionAgent forwards request to Broker**.
10. **Broker performs subscription authorization**.
    - If **auth fails**, Broker sends a **403 error** back to **SessionAgent**, which sends it to the **Client**.
    - If **auth succeeds**, proceed.
11. **Broker looks for an existing SubscriberAgent for the session and topic**.
    - If **SubscriberAgent exists**, send ACK back to **SessionAgent**, then to **Listener**, and then to **Client**.
    - Else, **SubscriberManager creates a new SubscriberAgent**.
12. **SubscriberAgent registers itself with the TopicManager**.
    - If **TopicAgent for the requested topic exists**, it adds the new subscriber.
    - If **TopicAgent does not exist**, request **TopicManager to create one**.
      - ⚠️ **Potential abuse case**: Prevent malicious users from sending rapid publish requests with random topic names.
13. **SubscriberAgent sends ACK to SessionAgent**.
14. **SessionAgent sends ACK to Client via Listener**.
    - If **Listener exists**, send ACK.
    - If **Listener does not exist**, log an error and wait for the listener to return or time out.

### **Error Handling**
- If **SubscriberAgent crashes**, **inform the client to re-subscribe**.
  - **Alternative**: Killing the session entirely is possible but **not ideal**.
- **Potential abuse case**: Clients attempting to rapidly subscribe to non-existent topics to exhaust system resources.

---

## **Publishing Flow**
1. **Client connects**.
2. **ListenerManager creates a new Listener**.
3. **Listener waits for a registration request**.
4. **Client sends a registration request**.
5. **Listener forwards registration request to Broker**.
6. **Broker performs authentication**.
   - If **auth fails**, send **403 error** back to the Listener, which forwards it to the Client.
   - If **auth succeeds**, proceed.
7. **SessionManager creates a new SessionAgent** and links it to the Listener.
8. **SessionAgent sends a registration ACK** to the **Client** via the Listener.
9. **Client sends a publish request** to the **SessionAgent**.
10. **SessionAgent forwards the request to the Broker**.
11. **Broker performs authorization**.
    - If **auth fails**, send **403 error** to **SessionAgent**, which forwards it to the **Client**.
    - If **auth succeeds**, proceed.
12. **Broker looks for an existing TopicAgent**.
    - If **TopicAgent exists**, Broker forwards the publish request.
    - If **TopicAgent does not exist**, Broker requests **TopicManager to create one**.
      - ⚠️ **Potential abuse case**: Malicious clients attempting to flood the system with publishes to fake topics.
13. **TopicAgent receives the publish request**.
    - **TopicAgent forwards notifications** to all registered **SubscriberAgents**.
14. **Each SubscriberAgent calls the SessionAgent with the message payload**.
15. **SessionAgent processes the message**:
    - If **SessionAgent replies successfully**, SubscriberAgent is unblocked.
    - If **SessionAgent does not reply in time**, it may be dead. Log an error and stop the SubscriberAgent.
    - If **SessionAgent replies with a failure**, **add the message to a dead-letter queue**.
16. **If Broker successfully forwards the publish request**:
    - **Broker sends PublishAck** back to the SessionAgent that made the request.
    - Otherwise, **send an error back to the SessionAgent**.
17. **SessionAgent forwards the PublishAck to the Client**.

### **Error Handling**
- **Use RPC to block while waiting for a reply from subscribers**.
  - If a **timeout** occurs, **requeue the message**.
- If a **SubscriberAgent crashes**, **inform the client to re-subscribe**.
  - **Alternative**: Killing the session entirely is possible but **not ideal**.
- **Potential abuse case**: Clients sending massive publish requests to unregistered topics, attempting to DOS the system.
