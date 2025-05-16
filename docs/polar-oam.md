## **Polar’s Open Application Model (OAM)** – Plain Language and Expanded

### **Overview**

Polar’s take on the Open Application Model brings a modular, extensible, and portable approach to application observability and deployment. the intention is to provide a consistent set of higher-level APIs that abstract away the details of infrastructure and environment, making it easier to monitor and manage applications no matter where or how they’re deployed.

This model supports both infrastructure and application operators, acknowledging that they have **distinct roles, scopes, and responsibilities**.

### **Roles and Separation of Concerns**

#### **Infrastructure Operators**

Infrastructure operators are responsible for provisioning and deploying services across a variety of environments, often on a global scale. Their work ensures that the foundational systems powering applications are reliable, secure, and highly available, regardless of the underlying infrastructure—whether it's cloud-based, on-premises, or hybrid. These operators are typically focused on the integrity of the deployment process itself: ensuring that services are correctly rolled out, monitored at the infrastructure level, and resilient to failure. To do this, they rely on tools like Kubernetes, Helm, and infrastructure-as-code frameworks to manage and automate deployments. Their responsibilities require a deep understanding of platform-level concerns, including network configuration, resource provisioning, and operational security. While they play a critical role in the lifecycle of applications, their domain stops at the infrastructure boundary; they enable the platform but do not dictate how the applications behave on top of it.

#### **Application Operators**

Application operators, by contrast, are often primarily concerned with the behavior and health of the applications once they are running on the platform. Their job goes beyond ensuring that an application is merely "up"—they are responsible for making sure it behaves correctly under real-world usage conditions. This includes monitoring for anomalies, adjusting runtime parameters, and fine-tuning performance to meet evolving user and business requirements. Unlike infrastructure operators, app operators typically do not manage the deployment infrastructure itself. However, they require sufficient autonomy to configure and control aspects of the application that are within their domain. They should be empowered to make changes to application-specific configurations without needing to route those requests through the infrastructure team. This division of responsibility not only reduces friction between roles but also enables faster iteration, better uptime, and a more resilient overall system by allowing each operator to act independently within clearly defined boundaries.

In many organizations—especially in highly regulated industries—it's common practice to isolate responsibilities so that developers remain unaware of deployment mechanics, while infrastructure teams avoid dealing with application concerns. While this separation can reduce complexity and risk on paper, it typically creates bottlenecks, misaligned incentives, and brittle handoffs when practiced at scale. As DevSecOps professionals, we advocate for shared ownership and collaboration across roles—but we also recognize the operational and compliance constraints that drive this separation. Therefore, our goal is to design a system that respects these boundaries while still enabling autonomy and seamless coordination. By explicitly acknowledging this divide, we can build tooling that bridges the gap without forcing either side to overstep their role, making it easier to meet regulatory requirements without sacrificing agility or clarity.

### **Empowering Operators Through Self-Service**

Polar’s philosophy emphasizes **self-service** and **autonomy**:

* App operators shouldn’t be blocked by constraints placed on them by third-party dependencies or red tape for common day-to-day changes.
* They should be able to safely tweak and manage their apps’ behavior directly through configuration—**without waiting on infra teams**.
* To enable this, the system clearly separates **deployment logic** (handled by infra) from **application logic** (owned by app operators).

---

### **The Deployment Policy Agent**

The **Deployment Policy Agent** is a purpose-built component of the Polar framework. It handles deployment orchestration and ensures consistency across environments.

#### **Key Responsibilities:**

* Consumes deployment configurations (e.g., Kubernetes YAML, Helm charts, VM image definitions).
* Enforces Polar’s standardized deployment model, regardless of the underlying tech.
* Enables infra operators to continue using familiar tools while seamlessly integrating with Polar’s ecosystem.
* Does **not** communicate with runtime systems like Cassini—it’s focused purely on **getting services deployed**, not on operating them after launch.

---

### **Smart Agents with Lazy Configuration**

When agents (e.g., observers or consumers) are deployed:

* They start in a **"dumb" state**—no configuration, no coordination.
* Once online, they **reach out to Cassini**, Polar’s central message broker, to request their runtime configuration from the configuration agent.

This separation ensures that:

* Deployment is **fast, minimal, and decoupled** from app-specific logic.
* Agents don’t need complex setup at the time of deployment—they’re configured **post-launch** by a different component.


### **Configuration Agent**

The **Configuration Agent** is responsible for:

* Serving up runtime configurations to deployed agents once they come online.
* Reading version-controlled, policy-based configurations (ideally written and maintained by app operators).
* Ensuring that runtime behavior is **predictable, auditable**, and **repeatable** based on source-controlled policies.

---

### **Summary: Flow of Control**

1. **Infra Operator** uses Helm/Kubernetes/other tools to deploy services using Polar’s framework.
2. **Deployment Policy Agent** handles these configurations and ensures that agents are launched properly.
3. Deployed agents come online in a passive state, with no internal logic or coordination yet.
4. Agents contact **Cassini**, the message broker, and request their runtime config.
5. **Configuration Agent** responds to these requests using versioned, operator-defined policies.


Polar is building for **collaboration without dependency**:

* Each operator (infra or app) should have the **independence to do their job well**.
* By separating deployment and runtime behavior, and offering policy-driven configuration, the platform provides the tools for secure, observable, and self-service operations at scale.
