# Diagnostic Vehicle Simulation & Reconstruction Platform

A configurable automotive diagnostic simulation platform for **reconstructing and simulating virtual vehicles from diagnostic specifications and real vehicle communication data**.

The platform is designed to simulate diagnostic communication without requiring physical ECUs or a complete vehicle. It can build a virtual vehicle from sources such as **ODX/PDX, DPDU diagnostic logs, CAN logs, PCAP/PCAPNG traces, ARXML, and other diagnostic/network artifacts**.

The goal is to provide a common environment for:

* Diagnostic software development
* ECU diagnostic testing
* DoIP/UDS testing
* CAN/ISO-TP testing
* Gateway and routing simulation
* Diagnostic regression testing
* Fault injection
* Log-based vehicle reconstruction
* Trace replay and analysis
* Virtual ECU development
* Automated diagnostic test execution

---

## 1. Vision

Modern vehicle diagnostics are distributed across multiple ECUs, gateways, networks, and diagnostic protocols.

A typical diagnostic environment may contain:

```text
                    Diagnostic Tester
                           |
                           |
                     Ethernet / CAN
                           |
                    +------+------+
                    |   Gateway   |
                    +------+------+
                           |
              +------------+------------+
              |            |            |
             CAN          CAN       Ethernet
              |            |            |
          +---+---+    +---+---+    +---+---+
          | ECU 1 |    | ECU 2 |    | ECU 3 |
          +-------+    +-------+    +-------+
```

Obtaining all of these physical components for development and testing is expensive and often impractical.

This project aims to provide a **Virtual Diagnostic Vehicle**:

```text
             Real Diagnostic Tester
                      |
                      |
               Virtual Vehicle
                      |
        +-------------+-------------+
        |             |             |
     Gateway         ECU          ECU
        |             |
       CAN          DoIP
        |             |
      ECU          ECU
```

The simulator should behave sufficiently like a real vehicle that an external diagnostic application can communicate with it using real diagnostic protocols.

---

# 2. Key Concept

The platform is based on three major capabilities:

```text
             Input Sources
                   |
                   v
        Vehicle Reconstruction
                   |
                   v
           Vehicle Model
                   |
                   v
          Simulation Engine
                   |
        +----------+----------+
        |          |          |
       CAN        DoIP       J1939
        |          |          |
      ISO-TP      TCP/IP    J1939 TP
        |          |
        +----------+
             |
            UDS
```

The same vehicle model can be created from different sources.

### Specification-driven

```text
ODX / PDX
   |
   v
Vehicle Model
   |
   v
Virtual Vehicle
```

### Log-driven

```text
PCAP / CAN / DPDU Logs
   |
   v
Log Analysis
   |
   v
Vehicle Reconstruction
   |
   v
Virtual Vehicle
```

### Hybrid

```text
ODX/PDX
   +
PCAP
   +
CAN Logs
   +
DPDU Logs
   +
ARXML
   +
Manual Configuration
          |
          v
   Unified Vehicle Model
          |
          v
     Virtual Vehicle
```

The **hybrid approach** is the primary long-term goal.

---

# 3. Core Objectives

## 3.1 Vehicle Reconstruction

Automatically discover and construct:

* ECUs
* Diagnostic endpoints
* Gateways
* CAN networks
* CAN-FD networks
* Ethernet networks
* DoIP nodes
* Diagnostic addresses
* CAN request/response IDs
* Diagnostic services
* DIDs
* DTCs
* Diagnostic sessions
* Routines
* Security access information
* Timing parameters
* Gateway routing paths

---

## 3.2 Virtual ECU Simulation

Each ECU should behave as an executable stateful virtual ECU.

Example:

```text
ECU
 |
 +-- Network Configuration
 |
 +-- Diagnostic Configuration
 |
 +-- Diagnostic Sessions
 |
 +-- UDS Services
 |
 +-- DIDs
 |
 +-- DTCs
 |
 +-- Routines
 |
 +-- Security Access
 |
 +-- Programming
 |
 +-- Timing
 |
 +-- Fault Behavior
```

The ECU should not simply replay packets.

It should maintain diagnostic state.

Example:

```text
Default Session
      |
      | 10 03
      v
Extended Session
      |
      | 27 01
      v
Security Access
      |
      | Key accepted
      v
Security Unlocked
```

---

# 4. Supported Input Sources

The architecture should support multiple sources.

## 4.1 ODX / PDX

Primary specification-based input.

Potential information:

* ECU identification
* Diagnostic services
* DIDs
* DTCs
* Diagnostic sessions
* Request/response definitions
* Parameters
* Coding
* Routines
* Security
* Communication parameters
* Flashing/programming information

Example:

```text
Vehicle.pdx
      |
      v
ODX Parser
      |
      v
Diagnostic Model
```

---

## 4.2 DPDU Diagnostic Logs

Diagnostic protocol logs can be used to reconstruct observed diagnostic behavior.

Example:

```text
Tester
 |
 | ReadDataByIdentifier
 | DID = F190
 v
ECU
 |
 | Response
 v
Tester
```

The system should extract:

* ECU identity
* Diagnostic address
* Service IDs
* Request parameters
* Response parameters
* Timing
* Positive responses
* Negative responses
* NRCs
* Session transitions
* Observed DIDs
* Observed routines
* Security sequences

---

## 4.3 CAN Logs

CAN and CAN-FD logs can be analyzed to identify:

* CAN IDs
* Request/response pairs
* ISO-TP traffic
* UDS communication
* ECU addresses
* Multi-frame messages
* Flow control
* Timing
* Diagnostic sessions

Example:

```text
0x7E0 -> 10 03
0x7E8 -> 50 03

0x7E0 -> 22 F1 90
0x7E8 -> 62 F1 90 ...
```

The system can infer:

```text
ECU
 |
 +-- Request ID: 0x7E0
 +-- Response ID: 0x7E8
 +-- Protocol: UDS over ISO-TP
 +-- Supported service: 0x10
 +-- Supported service: 0x22
 +-- Observed DID: F190
```

---

## 4.4 PCAP / PCAPNG

Ethernet traces can be analyzed for:

* TCP
* UDP
* DoIP
* SOME/IP
* Ethernet communication
* IP addresses
* DoIP logical addresses
* Routing activation
* Alive checks
* Diagnostic messages
* TCP connection behavior

Example:

```text
PCAP
 |
 +-- Ethernet
 |
 +-- IP
 |
 +-- TCP
 |
 +-- DoIP
 |
 +-- UDS
```

---

## 4.5 ARXML

ARXML support can provide:

* ECU information
* Network topology
* Ethernet configuration
* CAN configuration
* Service interfaces
* SOME/IP information
* Adaptive AUTOSAR configuration
* Diagnostic configuration

ARXML support should be added incrementally rather than making it a mandatory first input.

---

## 4.6 Manual Configuration

Users should also be able to create or modify vehicle elements manually.

Example:

```text
+ Add ECU

Name:
Engine_ECU

Protocol:
UDS

Transport:
DoIP

Logical Address:
0x1001

IP:
192.168.0.10
```

Manual configuration is particularly important when input artifacts are incomplete.

---

# 5. Unified Vehicle Model

All input sources should eventually be converted into a common internal representation.

```text
ODX
PDX
DPDU
PCAP
CAN
ARXML
Manual
       |
       v
+----------------------+
| Vehicle Model Builder|
+----------+-----------+
           |
           v
+----------------------+
| Unified Vehicle Model|
+----------------------+
```

The model should represent:

```text
Vehicle
 |
 +-- Networks
 |
 +-- Nodes
 |    |
 |    +-- ECU
 |    +-- Gateway
 |    +-- Tester
 |
 +-- Diagnostic Endpoints
 |
 +-- Routing
 |
 +-- Protocols
 |
 +-- Diagnostic Services
 |
 +-- DIDs
 |
 +-- DTCs
 |
 +-- Sessions
 |
 +-- Security
 |
 +-- Timing
```

---

# 6. Specification vs Observation

One of the most important concepts is distinguishing between **specified behavior** and **observed behavior**.

For example:

```text
ODX:

DID F190
Length = 17
```

Observed PCAP:

```text
DID F190
Response Length = 17
```

The model becomes:

```text
DID F190

Specification:
    Length = 17

Observed:
    Length = 17

Status:
    CONSISTENT
```

If the observed trace contains 20 bytes:

```text
DID F190

Specification:
    Length = 17

Observed:
    Length = 20

Status:
    MISMATCH
```

Possible reasons:

* ODX is outdated
* ECU software changed
* Vehicle variant differs
* Configuration mismatch
* Diagnostic implementation differs from specification

---

# 7. Confidence-Based Reconstruction

Not every piece of information can be known from a log.

The model should therefore maintain confidence.

Example:

```text
ECU 0x1001

Logical Address       100%  Confirmed
DoIP                  100%  Confirmed
UDS                   100%  Confirmed
DID F190              100%  Observed
DID F187               72%  Inferred
DID F188               64%  Inferred
Security Algorithm      0%  Unknown
```

Suggested states:

```text
CONFIRMED
OBSERVED
INFERRED
UNKNOWN
CONFLICT
```

This allows the system to construct partial vehicles without pretending that unknown information is known.

---

# 8. DoIP Simulation

DoIP should be a major part of the platform.

## Direct diagnostic communication

```text
Tester
 |
 | DoIP
 v
ECU
```

## Gateway-based communication

```text
Tester
 |
 | DoIP
 v
Gateway
 |
 | CAN
 v
ECU
```

## Multi-hop diagnostic routing

```text
Tester
 |
 v
Gateway A
 |
 v
Gateway B
 |
 v
ECU
```

The routing engine should therefore model diagnostic paths independently from the transport protocol.

Example:

```text
Target ECU: 0x1003

Path:

Tester
  |
  | Ethernet / DoIP
  v
Gateway
  |
  | CAN1 / ISO-TP
  v
ECU 0x1003
```

---

# 9. CAN Diagnostic Simulation

CAN should be implemented as a separate transport/network layer.

```text
UDS
 |
ISO-TP
 |
CAN
```

The simulator should support:

* CAN
* CAN-FD
* ISO-TP
* UDS
* Diagnostic request/response
* Multi-frame messages
* Flow control
* Timing
* CAN error simulation

---

# 10. J1939 Support

J1939 should be implemented independently from UDS.

```text
J1939
 |
 +-- PGN
 +-- SPN
 +-- Source Address
 +-- Destination Address
 +-- Transport Protocol
 +-- DM messages
 +-- DTC information
```

The architecture should allow additional protocols to be added later without changing the core vehicle model.

---

# 11. Gateway Simulation

Gateways are first-class simulation objects.

Example:

```text
                  Gateway
               /     |      \
              /      |       \
        Ethernet    CAN1     CAN2
           |         |        |
          DoIP      ECU1     ECU2
                    ECU3     ECU4
```

Routing rules should be configurable.

Example:

```text
DoIP Target 0x1001 -> CAN1 -> ECU1
DoIP Target 0x1002 -> CAN1 -> ECU2
DoIP Target 0x2001 -> CAN2 -> ECU3
```

---

# 12. Log-Based Vehicle Reconstruction

Logs should not only be used for replay.

The system should be able to analyze logs and generate a vehicle model.

Example:

```text
PCAP / CAN / DPDU
        |
        v
Protocol Detection
        |
        v
Message Decoding
        |
        v
Request/Response Correlation
        |
        v
ECU Discovery
        |
        v
Diagnostic Behavior Extraction
        |
        v
Vehicle Model
```

Then:

```text
[Build Virtual Vehicle]
```

creates executable simulation nodes.

---

# 13. Trace Replay

The reconstructed vehicle should support trace replay.

Example:

```text
Recorded Trace
      |
      v
Replay Engine
      |
      v
Virtual ECU
```

However, replay should not be limited to static packet playback.

The simulator should support **state-aware replay**.

Instead of:

```text
Request -> return recorded packet
```

the simulator should determine the response based on:

```text
Current ECU state
+
Incoming request
+
Timing
+
Scenario
+
Fault configuration
```

---

# 14. Fault Injection

A major feature of the platform should be controlled fault injection.

## Network faults

* Packet loss
* Packet delay
* Packet duplication
* Packet reordering
* TCP reset
* TCP disconnect
* Connection timeout
* Zero-window behavior
* High latency

## DoIP faults

* Routing activation rejection
* Invalid logical address
* Alive Check timeout
* Invalid DoIP header
* Invalid payload length
* Diagnostic message rejection

## UDS faults

* NRC injection
* Response timeout
* Response pending
* Invalid response
* Invalid length
* Session rejection
* Security failure

## ECU faults

* ECU reset
* Watchdog reset
* Diagnostic server failure
* Application failure
* DTC generation
* Communication loss

---

# 15. Scenario Engine

Faults should be configurable as scenarios.

Example:

```text
Scenario:
    ECU reset during flashing

Sequence:

Connect
    |
Routing Activation
    |
Programming Session
    |
Security Access
    |
Request Download
    |
Transfer Data
    |
Transfer Data
    |
RESET ECU
    |
TCP Disconnect
```

Scenarios should be deterministic and repeatable.

---

# 16. Simulation Modes

The platform should support several simulation modes.

### Specification Simulation

```text
ODX/PDX
   |
Vehicle Model
   |
Simulation
```

### Trace Simulation

```text
Log
 |
Behavior Extraction
 |
Simulation
```

### Hybrid Simulation

```text
ODX
 +
PCAP
 +
CAN
 +
DPDU
 +
Manual Overrides
      |
      v
Unified Vehicle Model
      |
      v
Simulation
```

### Replay Mode

```text
Recorded Trace
      |
      v
Replay
```

---

# 17. Proposed Architecture

```text
+-----------------------------------------------------------+
|                         UI / IDE                          |
|                                                           |
| Vehicle | Network | ECU | Gateway | Trace | Scenario     |
+-------------------------------+---------------------------+
                                |
+-------------------------------v---------------------------+
|                    Vehicle Model Layer                    |
|                                                           |
| ECU | Gateway | Network | Routing | Diagnostic Endpoint  |
+-------------------------------+---------------------------+
                                |
+-------------------------------v---------------------------+
|                Diagnostic Behavior Layer                  |
|                                                           |
| Sessions | Services | DIDs | DTCs | Security | Routines  |
+-------------------------------+---------------------------+
                                |
+-------------------------------v---------------------------+
|                    Protocol Engine                       |
|                                                           |
| UDS | DoIP | ISO-TP | CAN | CAN-FD | J1939 | TCP/IP      |
+-------------------------------+---------------------------+
                                |
+-------------------------------v---------------------------+
|                   Simulation Runtime                     |
|                                                           |
| State | Timing | Events | Scheduler | Fault Injection    |
+-------------------------------+---------------------------+
                                |
+-------------------------------v---------------------------+
|                   Communication Layer                    |
|                                                           |
| Virtual CAN | TCP/IP | Ethernet | Real CAN Interface     |
+-----------------------------------------------------------+


                 INPUT / RECONSTRUCTION LAYER

 ODX/PDX ----+
 ARXML ------+
 DPDU -------+
 PCAP -------+----> Parser / Extractor ---> Vehicle Model
 CAN Logs ---+
 DBC --------+
 Manual -----+
```

---

# 18. Vehicle Builder

The Vehicle Builder is responsible for combining all available information.

Example:

```text
Input:

Vehicle.pdx
vehicle.pcapng
diagnostic.dpdu
can_log.asc
vehicle.arxml
```

Output:

```text
Vehicle discovered

ECUs                    63
Gateways                 4
CAN Networks             6
Ethernet Networks        3
DoIP Nodes               17
Diagnostic Endpoints     42

UDS Services             81
DIDs                    1427
DTCs                     392

Confirmed                 82%
Inferred                  13%
Unknown                    5%

Conflicts                  7
```

The user can then review and resolve conflicts.

---

# 19. External Tester Integration

A key requirement is that external diagnostic applications should be able to communicate with the virtual vehicle.

Example:

```text
Real Diagnostic Tester
        |
        | TCP/IP / Ethernet
        |
        v
+-------------------------+
| Diagnostic Simulator    |
+-------------------------+
        |
        v
Virtual Gateway
        |
        v
Virtual ECU
```

The external tester should not need to know that the ECU is virtual.

This enables testing of:

* Diagnostic applications
* Flashing tools
* OTX procedures
* Automated test frameworks
* OEM diagnostic applications
* Development tools

---

# 20. Project Structure

A possible project structure:

```text
diagnostic-simulator/
│
├── README.md
│
├── docs/
│   ├── architecture/
│   ├── protocols/
│   ├── vehicle-model/
│   └── scenarios/
│
├── core/
│   ├── vehicle-model/
│   ├── ecu/
│   ├── gateway/
│   ├── network/
│   ├── routing/
│   └── simulation/
│
├── protocols/
│   ├── uds/
│   ├── doip/
│   ├── iso-tp/
│   ├── can/
│   ├── can-fd/
│   └── j1939/
│
├── parsers/
│   ├── odx/
│   ├── pdx/
│   ├── arxml/
│   ├── dpdu/
│   ├── pcap/
│   ├── can/
│   └── dbc/
│
├── reconstruction/
│   ├── ecu-discovery/
│   ├── protocol-detection/
│   ├── behavior-extraction/
│   ├── correlation/
│   ├── merge/
│   └── confidence/
│
├── simulation/
│   ├── runtime/
│   ├── scheduler/
│   ├── state-machine/
│   ├── fault-injection/
│   └── scenario-engine/
│
├── replay/
│
├── analysis/
│   ├── trace-analysis/
│   ├── comparison/
│   └── diagnostics/
│
├── ui/
│
└── tests/
```

---

# 21. Development Roadmap

## Phase 1 — Core Diagnostic Engine

* [ ] Vehicle model
* [ ] ECU model
* [ ] Diagnostic endpoint model
* [ ] UDS state machine
* [ ] UDS service engine
* [ ] Basic virtual ECU
* [ ] Simulation scheduler

## Phase 2 — CAN

* [ ] CAN transport abstraction
* [ ] ISO-TP
* [ ] UDS over CAN
* [ ] CAN request/response simulation
* [ ] CAN trace import
* [ ] CAN trace reconstruction

## Phase 3 — DoIP

* [ ] Ethernet abstraction
* [ ] TCP server
* [ ] DoIP implementation
* [ ] Vehicle Identification
* [ ] Routing Activation
* [ ] Alive Check
* [ ] Diagnostic Message
* [ ] Direct ECU communication
* [ ] Gateway-based communication

## Phase 4 — ODX/PDX

* [ ] PDX extraction
* [ ] ODX parsing
* [ ] Diagnostic service extraction
* [ ] DID extraction
* [ ] DTC extraction
* [ ] Session extraction
* [ ] ECU extraction
* [ ] ODX → Vehicle Model

## Phase 5 — Log Reconstruction

* [ ] PCAP parser
* [ ] DoIP detection
* [ ] UDS extraction
* [ ] CAN log parser
* [ ] ISO-TP detection
* [ ] DPDU parser
* [ ] Request/response correlation
* [ ] ECU discovery
* [ ] Behavior extraction
* [ ] Log → Vehicle Model

## Phase 6 — Gateway & Network

* [ ] Virtual gateway
* [ ] Routing rules
* [ ] Ethernet → CAN
* [ ] DoIP → CAN
* [ ] Multi-network topology
* [ ] Multi-hop routing

## Phase 7 — Fault Injection

* [ ] Timeout
* [ ] NRC injection
* [ ] ECU reset
* [ ] TCP disconnect
* [ ] Packet loss
* [ ] Packet delay
* [ ] Network failure
* [ ] Gateway failure

## Phase 8 — Advanced Protocols

* [ ] CAN-FD
* [ ] J1939
* [ ] SOME/IP
* [ ] SOME/IP-SD
* [ ] Additional automotive Ethernet protocols

## Phase 9 — Advanced Reconstruction

* [ ] ARXML import
* [ ] DBC import
* [ ] Multi-source model merging
* [ ] Specification vs observation comparison
* [ ] Confidence scoring
* [ ] Conflict resolution
* [ ] Vehicle version management

---

# 22. Long-Term Vision

The final platform should allow an engineer to start with whatever information is available.

### Only CAN log

```text
CAN Log
   ↓
Partial Vehicle
   ↓
Virtual ECU
```

### Only PCAP

```text
PCAP
 ↓
DoIP discovery
 ↓
Virtual Vehicle
```

### Only ODX

```text
ODX/PDX
 ↓
Diagnostic Vehicle
```

### ODX + PCAP

```text
ODX
 +
PCAP
 ↓
Specification + Observation
 ↓
Validated Vehicle Model
```

### Complete project

```text
ODX
PDX
ARXML
PCAP
CAN Logs
DPDU Logs
DBC
OTX
Manual Configuration
        |
        v
+--------------------------+
| Unified Vehicle Model    |
+--------------------------+
        |
        v
+--------------------------+
| Executable Virtual       |
| Vehicle                  |
+--------------------------+
        |
        +--------+---------+
        |        |         |
       CAN      DoIP      J1939
        |        |         |
        +--------+---------+
                 |
                 v
        External Diagnostic
              Tester
```

---

# 23. Core Design Principle

The platform should follow one fundamental principle:

> **Do not build separate simulators for ODX, CAN logs, PCAP, and DPDU. Build one Vehicle Model and provide multiple ways to populate it.**

Therefore:

```text
ODX/PDX -----------+
CAN Logs ----------+
DPDU Logs ---------+
PCAP --------------+
ARXML -------------+----> Unified Vehicle Model
DBC ---------------+
Manual ------------+
                         |
                         v
                   Simulation Engine
```

This makes the architecture extensible and prevents the simulator from becoming a collection of unrelated protocol tools.

---

# 24. Initial MVP

The recommended first MVP is intentionally narrow:

```text
                    MVP
                     |
          +----------+----------+
          |                     |
        ODX/PDX              PCAP/CAN
          |                     |
          +----------+----------+
                     |
                     v
             Vehicle Model
                     |
                     v
                 Virtual ECU
                     |
             +-------+-------+
             |               |
            CAN             DoIP
             |               |
           ISO-TP            TCP
             |               |
             +-------+-------+
                     |
                    UDS
                     |
                     v
             External Tester
```

The first successful demonstration should be:

1. Import an ODX/PDX.
2. Discover the ECU and diagnostic services.
3. Generate a virtual ECU.
4. Start the virtual ECU.
5. Connect an external diagnostic tester.
6. Perform UDS communication.
7. Capture the resulting trace.
8. Inject a diagnostic/network fault.
9. Observe the tester's behavior.
10. Import an existing trace and reconstruct/update the same ECU.

Once this works, the platform has a solid foundation for the larger vehicle reconstruction vision.

---

## 25. End Goal

The ultimate goal is to make the following workflow possible:

```text
               "I have some vehicle data."
                         |
                         v
        +--------------------------------+
        | Import available artifacts     |
        |                                |
        | ODX / PDX                      |
        | ARXML                          |
        | CAN Logs                       |
        | DPDU Logs                      |
        | PCAP / PCAPNG                  |
        | DBC                            |
        | OTX                            |
        +----------------+---------------+
                         |
                         v
               Vehicle Reconstruction
                         |
                         v
                Conflict Resolution
                         |
                         v
                 Virtual Vehicle
                         |
              +----------+----------+
              |          |          |
             CAN        DoIP       J1939
              |          |          |
              +----------+----------+
                         |
                         v
                 Diagnostic Tester
                         |
                         v
                 Test / Debug / Fault
```

The platform is therefore not intended to be only a **diagnostic protocol simulator**.

It is intended to become a **reconstructable, executable virtual vehicle platform** where specifications, logs, traces, and manually defined behavior can all contribute to the same vehicle model.
