

-- A test plan is just a list of producers
-- TODO: We've discussed potentially splitting the producer and sink components again to enable them to communicate over the wire
-- This is probably where some of those configurations would be best suited.
--let ProducerConfig = {
--    producers : List Producer
--}

-- Environment definition to dertermine which dependencies to activiate as part of any given test scenario
--
let Environment = {
    -- TOOD: enable cassini configuration to be included
    cassini:
        {
        tag: Text
        -- paths to mount TLS certificates
        , ca_cert_path: Text
        , server_cert_path: Text
        , server_key_path: Text
        , jager_host: Optional Text
        , log_level: Optional Text
        }

    -- Neo4j graph configureation
    , neo4j :
        { enable : Bool
        , version : Text
        , config: Optional Text
        }
        -- potential fake OCI or package registry
        , registry :
        { enable : Bool
        , version : Text
        , port : Natural
        }
}

--
--let ClientTlsConfig = {
--    ca_cert_path: Text
--    , client_cert_path: Text
--    , client_key_path: Text
--}

-- Union definiiton for what kind of agent to start during each test
-- ------warning-----
-- We're using rkyv to re-serailsze and deserialize within the message broker and the client,
-- so when we go to derive the Archive trait, we get a compile error that the type reference is "ambigous" in the rust
-- Enum implentnation, so we suffix agents with "Agent", so far, this was only the case for the resolver, but conventions are nice.
let Agent = < ProducerAgent
| SinkAgent
| ResolverAgent
| GitlabConsumer
| KubeConsumer
| LinkerAgent
| Observer
>

-- Union definition for the set of actions the harness controller can take during each test.
-- Which agents it should start,  what events the producer should emit, etc.
-- Sleep - Sleep for an optionally provided amount of time,
-- CAUTION!!! This will sleep the harness indefinitely and await shutdown from actors unless timeout is provided.
-- config : Text is intentionally opaque; the controller interprets it (could be TOML, YAML, JSON, Dhall itself).
-- EmitGitlabEvent and EmitKubeEvent allow injecting stimuli without running real systems.
-- AssertGraph delegates correctness checking to Cypher queries.
-- AssertEvent lets the sink verify that an expected number of provenance events passed through.
let Action =
< StartAgent : { agent : Agent, config : Optional Text }
| EmitGitlabEvent : { json : Text }
| EmitKubeEvent : { json : Text }
| Sleep : { duration: Optional Natural }
| AssertGraph : { cypher : Text, expectRows : Natural }
| AssertEvent : { topic : Text, count : Natural }
>

-- Denotes what phase of the test we're in, should we decide to have multiple phases
-- Setup, execute, teardown, validate etc.let TestPhase =
let Phase = { name : Text
, actions : List Action
}


let TestPlan =
    { environment : Environment
    , phases : List Phase
    }

in
{ Environment, Phase, Agent, Action, TestPlan }
