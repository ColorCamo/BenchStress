pub mod stressors;
pub mod sensors;
mod reporting;
mod components;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize};
use std::sync::atomic::Ordering::Relaxed;
use std::thread;
use std::time::{Duration, Instant};
use colored::Colorize;
use ocl::{Device, DeviceType, Platform};
use requestty::{Question};
use sysinfo::{CpuExt, System, SystemExt};
// use crate::components::GreetingValues;
use crate::reporting::watch_in_background;
use crate::stressors::*;

fn main() {
    let mut sys = System::new_all();
    let mut gpu_ctx: Option<OpenCLContext> = None;
    let mut gpu_device: Option<Device> = None;
    // let system_information = GreetingValues::new(&sys);


    // // Display system information:s
    // println!("Hello {}!", system_information.host_name);
    // println!("Memory: {:?} GBs", system_information.memory);
    // println!("CPU Information: {}", system_information.cpu_information);

    println!("OS: {} v{}", sys.long_os_version().unwrap_or_else(|| "N/A".to_string()), sys.kernel_version().unwrap_or_else(|| "N/A".to_string()));
    println!("Current CPU: {}", sys.cpus()[0].brand());
    println!("CPU Information: {:?} cores & {:?} threads", sys.physical_core_count().unwrap_or(0),  sys.cpus().len());

    const STRESSORS: [&str; 6] = ["Fibonacci", "Primes", "Matrix Multiplication", "Float Addition", "Float Multiplication", "Square Root"];

    loop {
        let questions = [
            Question::select("main_question")
                .message("What would you like to test?")
                .choices(["CPU", "GPU", "All (Separate)", "All (Together)"])
                .build(),
            Question::select("gpu_select")
                .message("What GPU would you like to use")
                .choices(get_gpu_options().into_iter().map(|i| i.name().unwrap()))
                .build(),
            Question::select("cpu_question")
                .message("How many CPU(s) would you like to use?")
                .choices((1..=sys.cpus().len()).map(|cpu| format!("{cpu} CPU(s)")).collect::<Vec<String>>())
                .when(|ans: &requestty::Answers| {
                    ans.get("main_question")
                        .expect("Main question was not found. This should not have happened")
                        .as_list_item()
                        .expect("Type of the Main question was not a ListItem.. This should not have happened!")
                        .index as i32 == 0
                })
                .build(),
            Question::multi_select("how_test")
                .message("How would you like the test to terminate? (Will terminate when any condition is met in this order)")
                .choices(get_termination_options(&mut sys))
                .validate(|ans, _ | {
                    if ans.iter().filter(|&&a| a).count() < 1 {
                        Err("You must choose at least one option.".into())
                    } else {
                        Ok(())
                    }
                })
                .build(),
            Question::int("duration")
                .message("How long would you like the test to be? (In Minutes)")
                .when(|ans: &requestty::Answers| {
                    ans.get("how_test")
                        .expect("The 'How would you like to terminate question' could not be found. This should not have happened!")
                        .as_list_items()
                        .expect("Type of the 'How would you like to terminate' question has been changed. This should not have happened!")
                        .iter()
                        .any(|li| li.index == 0)
                })
                .default(1)
                // lol why not
                .validate_on_key(|time, _| time > 0 && time < i64::MAX)
                .validate(|time, _| {
                    if time > 0 && time < i64::MAX {
                        Ok(())
                    } else {
                        Err("Time must be greater than 0 and less than the maximum value of an i64".into())
                    }
                })
                .build(),
            Question::int("temperature")
                .message("What temperature would you like to stop at? (In Celsius)")
                .when(|ans: &requestty::Answers| {
                    ans.get("how_test")
                        .expect("The 'How would you like to terminate question' could not be found. This should not have happened!")
                        .as_list_items()
                        .expect("Type of the 'How would you like to terminate' question has been changed. This should not have happened!")
                        .iter()
                        .any(|li| li.text == *"Temperature")
                })
                .default(90)
                .validate_on_key(|temp, _| temp > 0 && temp < 150)
                .validate(|temp, _| {
                    let current_temp = sensors::cpu_temp(&mut sys, true);
                    if (current_temp.is_some() && current_temp.unwrap() > temp as f32) || temp >= 150 {
                        let error_message = if temp > 150 {
                            format!("Temperature must be less than 150 degrees Celsius. Current temperature is {} degrees Celsius.", current_temp.unwrap())
                        } else {
                            format!("Temperature must be greater than the current temperature of {} degrees Celsius.", current_temp.unwrap())
                        };
                        Err(error_message)
                    } else {
                        Ok(())
                    }
                })
                .build(),
            Question::select("method")
                .message("What method would you like to use?")
                .choices(STRESSORS)
                .build(),
        ];

        let answers = requestty::prompt(questions)
            .expect("Couldnt get the answers. Something went terrible wrong.");

        let chosen_index = answers.get("main_question")
            .expect("Main question was not found. This should not have happened")
            .as_list_item()
            .map(|list_item| list_item.index)
            .expect("Didnt get an option for the main question") as i32;

        let duration = answers.get("duration")
            .and_then(|d| d.as_int())
            .map(|duration| Duration::from_secs(duration as u64 * 60));

        let temperature = answers.get("temperature")
            .and_then(|t| t.as_int());

        let method = answers.get("method")
            .and_then(|d| d.as_list_item())
            .map(|method| STRESSORS[method.index])
            .unwrap_or(STRESSORS[0]);

        if chosen_index == 0
        {
            let cpus = &answers.get("cpu_question")
                .expect("CPU Option was chosen and no cpu count was given. We gotta go bye bye.")
                .as_list_item()
                .expect("Type of 'How many CPU(s) question was changed'. This should not have happened")
                .index + 1;

            match do_cpu_work(method, cpus, temperature, duration, &mut sys) {
                Ok(job) => println!("{job}"),
                Err(e) => println!("{e}"),
            }
        }
        else if chosen_index == 1
        {
            println!("gpu here");
            let gpu_name = &answers.get("gpu_select")
                .expect("GPU Option was chosen and no gpu name was given. We gotta go bye bye.")
                .as_list_item()
                .expect("Type of 'Which GPU question was changed'. This should not have happened")
                .index;


            gpu_ctx = Some(OpenCLContext::new(Device::first(Platform::default()).unwrap()).unwrap());


            let s = gpu_ctx.unwrap();
            let g = get_opencl_program("float_add", &s).unwrap();

            do_gpu_work(g)

        }




        let answer = Question::confirm("test_rerun")
            .message("Would you like to run another test?")
            .default(true)
            .build();

        let prompt = requestty::prompt([answer])
            .expect("Couldnt get the answers. Something terrible went wrong.");
        let rerun = prompt.get("test_rerun")
            .expect("Couldnt get the rerun answer. Something terrible went wrong.")
            .as_bool()
            .expect("Couldnt get the rerun answer. Something terrible went wrong.");

        if !rerun { break; }
    }
}

fn get_termination_options(sys: &mut System) -> Vec<String> {
    let mut options = Vec::new();
    options.push("Time".to_string());
    if sensors::cpu_temp(sys, false).is_some()  {
        options.push("Temperature".to_string());
    }
    options
}

fn get_stressor_functions(
    stressor: &str
) -> fn() {
    match stressor {
        "Fibonacci" => fibonacci_cpu,
        "Primes" => primes,
        "Matrix Multiplication" => matrix_multiplication,
        "Float Addition" => float_add,
        "Float Multiplication" => float_mul,
        "Square Root" => sqrt_cpu,
        _ => panic!("Invalid stressor function")
    }
}

pub fn get_opencl_program(
    method: &str,
    ctx: &OpenCLContext,
) -> Result<OpenCLProgram, String> {
    match method {
                                                                                                                            //         // result vector
        "sqrt" => {
            // yeah, lets spam sqrt f32 on gpu
            let sqrt_vector = [952_f32; 1000];
            let result_vector = [0_f32; 1000];
            OpenCLProgram::new(ctx, OPENCL_SQUARE_ROOT, "sqrt", &[sqrt_vector, result_vector])
        },
        "float_add" => {
            let f_add_vector = [952.139_1_f32; 1000];
            let result_vector = [0_f32; 1000];
            OpenCLProgram::new(ctx, OPENCL_FLOAT_ADD, "float_add", &[f_add_vector, result_vector])
        },
        "matrix_mult" => {
            let matrix_a = [201.139_13_f32; 1000];
            let matrix_b = [952.139_1_f32; 1000];
            let result_vector = [0_f32; 1000];
            OpenCLProgram::new(ctx, OPENCL_MATRIX_MULTIPLICATION, "matrix_mult", &[matrix_a, matrix_b, result_vector])
        },
        // "fibonacci" => OpenCLProgram::new(ctx, OPENCL_FIBONACCI, "fibonacci"),
        // "factorial" => OpenCLProgram::new(ctx, OPENCL_FACTORIAL, "factorial"),
        // "primes" => OpenCLProgram::new(ctx, OPENCL_PRIMES, "primes"),
        _ => {
            println!("No method found, defaulting to sqrt");
            let sqrt_vector = [952_f32; 1000];
            let result_vector = [0_f32; 1000];
            OpenCLProgram::new(ctx, OPENCL_SQUARE_ROOT, "sqrt", &[sqrt_vector, result_vector])
        }
    }
}

fn get_gpu_options() -> Vec<Device> {
    let platform = Platform::default();
    Device::list(platform, Some(DeviceType::GPU)).unwrap()
}

fn do_gpu_work(
    program: OpenCLProgram,
) {
    loop {
        program.run();
    }




}


fn do_cpu_work(
    method: &str,
    cpu_count: usize,
    stop_temperature: Option<i64>,
    duration: Option<Duration>,
    system: &mut System,
) -> Result<Job, String> {
    let start_time = Instant::now();
    let running = Arc::new(AtomicUsize::new(0));

    let atomic_bool = running.clone();
    let function = get_stressor_functions(method);

    println!("{}", format!("???? Starting {method}. If you wish to stop the test at any point hold Control+C").white().bold());


    thread::scope(move |scope| {
        let mut handles = Vec::with_capacity(cpu_count);
        for _ in 0..cpu_count
        {
            let thread_running = running.clone();
            let handle = scope.spawn(move ||
                {
                    let mut iterations: u64 = 0;
                    while thread_running.load(Relaxed) == 0
                    {
                        function();
                        iterations += 1;
                    }
                    iterations
                });
            handles.push(handle);
        }


        let background_report = watch_in_background(
            stop_temperature,
            duration,
            system,
            start_time,
            atomic_bool,
        );

        let stop_reason = match running.load(Relaxed) {
            1 => "Time Limit exceeded",
            2 => "Temperature exceeded",
            3 => "Ctrl-C caught",
            _ => panic!("This should have never happened. {} is not a valid option", running.load(Relaxed))
        }.to_string();

        let mut total_iterations = 0;
        for handle in handles {
            if let Ok(iterations) = handle.join() {
                total_iterations += iterations;
            } else {
                return Err("Failed to join thread".to_string());
            }
        }


        Ok(
            Job {
                name: method.to_string(),
                total_iterations,
                cpu_count,
                stop_reasoning: stop_reason,
                average_cpu_temp: background_report.average_cpu_temp,
                min_cpu_temp: background_report.min_cpu_temp,
                max_cpu_temp: background_report.max_cpu_temp,
            }
        )
    })
}


pub struct Job {
    name: String,
    total_iterations: u64,
    cpu_count: usize,
    average_cpu_temp: Option<f32>,
    min_cpu_temp: Option<f32>,
    max_cpu_temp: Option<f32>,
    stop_reasoning: String
}

impl std::fmt::Display for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "\n{} Stress Test Results ", self.name)?;

        write!(f, "\n??? Job Name: {} \n??? Total Iterations: {} \n??? CPU Count: {} \n??? Stop Reasoning: {}",
               self.name, pretty_print_int(self.total_iterations), self.cpu_count, self.stop_reasoning)?;

        if let Some(max_temp) = self.max_cpu_temp {
            write!(f, "\n??? Maximum CPU Temperature: {max_temp:.2}??C")?;
        }

        if let Some(min_temp) = self.min_cpu_temp {
            write!(f, "\n??? Max CPU Temperature: {min_temp:.2}??C")?;
        }

        if let Some(average_temp) = self.average_cpu_temp {
            write!(f, "\n??? Max CPU Temperature: {average_temp:.2}??C")?;
        }


        Ok(())
    }
}

fn pretty_print_int(i: u64) -> String {
    let mut s = String::new();
    let i_str = i.to_string();
    let a = i_str.chars().rev().enumerate();
    for (idx, val) in a {
        if idx != 0 && idx % 3 == 0 {
            s.insert(0, ',');
        }
        s.insert(0, val);
    }
    s
}
