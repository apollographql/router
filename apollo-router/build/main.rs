mod spaceport;
mod uplink;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    uplink::main();
    spaceport::main()
}
