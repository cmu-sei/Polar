@0xf78f09d7aea24a7c;

struct GitlabData {
  union {
    users @0 :List(User);
    projects @1 :List(Project);
    groups @2 :List(UserGroup);
    projectUsers @3 :UserLink;
    projectRunners @4 :RunnerLink;
    groupMembers @5 :UserLink;
    groupRunners @6 :RunnerLink;
    groupProjects @7 :ProjectLink;
    runners @8 :List(Runner);
    jobs @9 :List(Job);
    pipelines @10 :List(Pipeline);
  }
}



struct Project {
  id @0 :UInt32;
  name @1 :Text;
  description @2 :Text;
  creatorId @3 :UInt32;
  namespace @4 :Namespace;
  lastActivityAt @5 :Text;
}

struct Namespace {
  id @0 :UInt32;
  parentId @1 :UInt32;
  name @2 :Text;
  fullPath @3 :Text;
  kind @4 :Text;
  webUrl @5 :Text;
}

struct User {
  id @0 :UInt32;
  username @1 :Text;
  name @2 :Text;
  state @3 :Text;
  createdAt @4 :Text;
  isAdmin @5 :Bool;
  lastSignInAt @6 :Text; 
  currentSignInAt @7 :Text;
  currentSignInIp @8 :Text;
  lastSignInIp @9 :Text;
}

struct Runner {
  id @0 :UInt32;
  paused @1 :Bool;
  isShared @2 :Bool;
  description @3 :Text;
  ipAddress @4 :Text;
  runnerType @5 :Text;
  name @6 :Text;
  online @7 :Bool;
  status @8 :Text;
}

struct UserGroup {
  id @0 :UInt32;
  fullName @1 :Text;
  description @2 :Text;
  visibility @3 :Text;
  parentId @4 :UInt32;
  createdAt @5 :Text;
}

struct Pipeline {
  id @0 :UInt32;
  projectId @1 :UInt32;
  status @2 :Text;
  source @3 :Text;
  sha @4 :Text;
  createdAt @5 :Text;
  updatedAt @6 :Text;
}

struct Job {
  id @0 :UInt32;
  ipAddress @1 :Text;
  status @2 :Text;
  stage @3 :Text;
  name @4 :Text;
  createdAt @5 :Text;
  startedAt @6 :Text;
  finishedAt @7 :Text;
  erasedAt @8 :Text;
  duration @9 :Float64;
  allowFailure @10 :Bool;
  webUrl @11 :Text;
  failureReason @12 :Text;
  runner @13 :Runner;
}

struct GitCommit {
  id @0 :Text;
  shortId @1 :Text;
  title @2 :Text;
  createdAt @3 :Text;
  parentIds @4 :List(Text);
  message @5 :Text;
  authorName @6 :Text;
  authoredDate @7 :Text;
  committedDate @8 :Text;
}

struct ContainerRegistry {
  id @0 :UInt32;
  name @1 :Text;
  path @2 :Text;
  projectId @3 :UInt32;
  createdAt @4 :Text;
  cleanupPolicyStartedAt @5 :Text;
}

# datatypes to represent linkages between resources by ID
struct UserLink {
  resourceId @0 :UInt32;
  resourceList @1 :List(User);
}

struct GroupLink {
  resourceId @0 :UInt32;
  resourceList @1 :List(UserGroup);
}

struct RunnerLink {
  resourceId @0 :UInt32;
  resourceList @1 :List(Runner);
}

struct ProjectLink {
  resourceId @0 :UInt32;
  resourceList @1 :List(Project);
}


struct UserList {
  users @0 :List(User);
}

struct ProjectList {
  projects @0 :List(Project);
}
struct NamespaceList {
  namespaces @0 :List(Namespace);
}
struct UserGroupList {
  groups @0 :List(UserGroup);
}
struct RunnerList {
  runners @0 :List(Runner);
}
struct PipelineList {
  pipelines @0 :List(Pipeline);
}
struct JobList {
  jobs @0 :List(Job);
}