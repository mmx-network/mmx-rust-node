// Native text/config oracle. No Rust normalization or consensus logic is used.
#include <mmx/ChainParams.hxx>
#include <mmx/Transaction.hxx>
#include <vnx/vnx.h>
#include <fstream>
#include <sstream>

template<class T> std::string wire(std::shared_ptr<T> value) {
    std::vector<uint8_t> bytes;
    vnx::VectorOutputStream stream(&bytes);
    vnx::TypeOutput out(&stream);
    vnx::write(out, value);
    out.flush();
    return vnx::to_hex_string(bytes.data(), bytes.size());
}
int main(int argc, char** argv) {
    if(argc != 3) return 1;
    vnx::Object row;
    try {
        const std::string kind = argv[1];
        if(kind == "config") {
            vnx::read_config_tree(argv[2]);
            auto params = mmx::ChainParams::create();
            vnx::read_config("chain.params", params);
            row["wire"] = wire(params);
        } else {
            std::ifstream file(argv[2]);
            std::string text((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
            if(kind == "params") {
                auto params = mmx::ChainParams::create();
                vnx::from_string(text, *params);
                row["wire"] = wire(params);
            } else {
                std::shared_ptr<const mmx::Transaction> tx;
                vnx::from_string(text, tx);
                if(!tx) throw std::logic_error("null transaction");
                row["wire"] = wire(tx);
            }
        }
        row["result"] = "OK";
    } catch(const std::exception& e) {
        row["result"] = e.what();
    }
    std::cout << vnx::to_string(row) << '\n';
}
