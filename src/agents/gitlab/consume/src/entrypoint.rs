// Polar
// Copyright 2023 Carnegie Mellon University.
// NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.
// [DISTRIBUTION STATEMENT D] Distribution authorized to the Department of Defense and U.S. DoD contractors only (materials contain software documentation) (determination date: 2022-05-20). Other requests shall be referred to Defense Threat Reduction Agency.
// Notice to DoD Subcontractors:  This document may contain Covered Defense Information (CDI).  Handling of this information is subject to the controls identified in DFARS 252.204-7012 – SAFEGUARDING COVERED DEFENSE INFORMATION AND CYBER INCIDENT REPORTING
// Carnegie Mellon® is registered in the U.S. Patent and Trademark Office by Carnegie Mellon University.
// This Software includes and/or makes use of Third-Party Software subject to its own license, see license.txt file for more information. 
// DM23-0821
// 
use std::error::Error;
use std::process::Command;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error> > {
    //try spawning consumers
    Command::new("./projects_consumer").spawn().expect("Could not execute gitlab project consumer binary.");
    Command::new("./users_consumer").spawn().expect("Could not execute gitlab user consumer binary.");
    Command::new("./groups_consumer").spawn().expect("Could not execute gitlab group consumer binary.");
    Command::new("./runners_consumer").spawn().expect("Could not execute gitlab runners consumer binary.");
    Ok(())
}
