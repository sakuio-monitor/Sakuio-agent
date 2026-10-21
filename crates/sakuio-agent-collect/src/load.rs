use sakuio_protocol::LoadReport;
use sysinfo::System;

pub fn load_averages() -> LoadReport {
    let avg = System::load_average();
    LoadReport {
        load1: avg.one,
        load5: avg.five,
        load15: avg.fifteen,
    }
}
