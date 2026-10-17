// Execute the same command traces in C++ and Rust, including actual StorageDB
// reads, heap addresses, failures, gas, and every successful persistence write.
#include <mmx/vm/Engine.h>
#include <mmx/vm/StorageDB.h>
#include <mmx/signature_t.hpp>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <sstream>

using namespace mmx;
using namespace mmx::vm;

std::string raw(const var_t* var, bool flags = false) {
    if(!var) return "-";
    auto data = serialize(*var, false, flags);
    std::ostringstream out;
    out << std::hex << std::setfill('0');
    for(size_t i = 0; i < data.second; ++i) out << std::setw(2) << unsigned(data.first[i]);
    return out.str();
}

struct Recorder : Storage {
    std::shared_ptr<StorageDB> db;
    mutable std::string writes;
    std::map<addr_t, uint128> balances;
    Recorder(const std::string& path, std::shared_ptr<DataBase> database) : db(std::make_shared<StorageDB>(path, database)) {}
    std::unique_ptr<var_t> read(const addr_t& c, uint64_t a) const override { return db->read(c, a); }
    std::unique_ptr<var_t> read(const addr_t& c, uint64_t a, uint64_t k) const override { return db->read(c, a, k); }
    uint64_t lookup(const addr_t& c, const var_t& value) const override { return db->lookup(c, value); }
    std::unique_ptr<uint128> get_balance(const addr_t&, const addr_t& currency) override {
        const auto i = balances.find(currency);
        return i == balances.end() ? nullptr : std::make_unique<uint128>(i->second);
    }
    void write(const addr_t& c, uint64_t a, const var_t& v) override {
        db->write(c, a, v);
        writes += std::to_string(a) + ",-," + raw(&v, true) + ";";
    }
    void write(const addr_t& c, uint64_t a, uint64_t k, const var_t& v) override {
        db->write(c, a, k, v);
        writes += std::to_string(a) + "," + std::to_string(k) + "," + raw(&v, true) + ";";
    }
};

std::vector<std::string> split(const std::string& text, char delimiter) {
    std::vector<std::string> out;
    std::stringstream in(text); std::string value;
    while(std::getline(in, value, delimiter)) out.push_back(value);
    return out;
}

struct Runner {
    std::shared_ptr<DataBase> database;
    std::shared_ptr<Recorder> storage;
    std::unique_ptr<Engine> vm;
    std::map<std::string, uint64_t> aliases;
    std::string effects;
    Runner(const std::string& path) {
        std::filesystem::create_directories(path);
        database = std::make_shared<DataBase>(1);
        storage = std::make_shared<Recorder>(path + '/', database);
    }
    uint64_t address(const std::string& text) {
        const auto plus = text.find('+');
        if(plus != std::string::npos) return address(text.substr(0, plus)) + address(text.substr(plus + 1));
        if(text == "S") return MEM_STATIC;
        if(text == "T") return MEM_STACK;
        if(text == "H") return MEM_HEAP;
        if(text == "E") return MEM_EXTERN;
        if(text == "N") return MEM_HEAP + GLOBAL_NEXT_ALLOC;
        if(text[0] == '@') return aliases.at(text.substr(1));
        return std::stoull(text, nullptr, 0);
    }
    std::unique_ptr<var_t> value(const std::string& text) {
        if(text == "n") return std::make_unique<var_t>();
        if(text == "t") return std::make_unique<var_t>(true);
        if(text == "f") return std::make_unique<var_t>(false);
        if(text == "m") return std::make_unique<map_t>();
        const auto data = text.substr(2);
        switch(text[0]) {
            case 'u': return std::make_unique<uint_t>(data.substr(0, 2) == "0x" ? uint256_t(data.substr(2).c_str(), 16) : uint256_t(data.c_str(), 10));
            case 'r': return std::make_unique<ref_t>(address(data));
            case 'a': return std::make_unique<array_t>(address(data));
            case 's': case 'b': {
                std::vector<uint8_t> bytes;
                for(size_t i = 0; i < data.size(); i += 2) bytes.push_back(std::stoul(data.substr(i, 2), nullptr, 16));
                return binary_t::alloc(bytes.data(), bytes.size(), text[0] == 's' ? TYPE_STRING : TYPE_BINARY);
            }
        }
        throw std::logic_error("bad fixture value: " + text);
    }
    void run(const std::string& program) {
        vm->code.clear();
        for(const auto& text : split(program, ';')) {
            const auto fields = split(text, ',');
            bool found = false;
            for(unsigned i = 0; i < 256; ++i) {
                if(get_opcode_info(opcode_e(i)).name != "OP_" + fields[0]) continue;
                instr_t instr(opcode_e(i), address(fields[1]));
                uint32_t* operands[] = {&instr.a, &instr.b, &instr.c, &instr.d};
                for(size_t n = 2; n < fields.size(); ++n) *operands[n - 2] = address(fields[n]);
                vm->code.push_back(instr); found = true; break;
            }
            if(!found) throw std::logic_error("bad fixture opcode");
        }
        vm->begin(0); vm->run();
    }
    std::string execute(const std::string& command) {
        const auto f = split(command, ' ');
        const auto& op = f[0];
        if(op == "new") { vm = std::make_unique<Engine>(addr_t(), storage, f[1] == "1", 1); vm->gas_limit = 1000000; effects.clear(); }
        else if(op == "balance") storage->balances[((binary_t&)*value(f[1])).to_addr()] = uint128(uint256_t(f[2].c_str(), 10));
        else if(op == "hooks") {
            if(f[1] == "1") {
                vm->log_func = [this](uint32_t level, const std::string& message) {
                    const auto v = binary_t::alloc(message); effects += "L," + std::to_string(level) + "," + raw(v.get()) + ";";
                };
                vm->event_func = [this](const std::string& name, uint64_t data) {
                    const auto v = binary_t::alloc(name); effects += "E," + raw(v.get()) + "," + raw(vm->read(data)) + ";";
                };
                vm->remote_call = [this](const std::string& name, const std::string& method, uint32_t nargs) {
                    effects += "R," + raw(binary_t::alloc(name).get()) + "," + raw(binary_t::alloc(method).get()) + "," + std::to_string(nargs) + ";";
                    const auto stack = MEM_STACK + vm->get_frame().stack_ptr;
                    if(method == "fail") throw std::logic_error("host rejected call");
                    if(nargs) vm->copy(stack, stack + 1);
                    else vm->write(stack, uint_t(42));
                };
                vm->read_contract = [this](const addr_t& address, const std::string& field, uint64_t dst) {
                    effects += "C," + raw(to_binary(address).get()) + "," + raw(binary_t::alloc(field).get()) + ";";
                    vm->write(dst, uint_t(1337));
                };
            } else { vm->log_func = {}; vm->event_func = {}; vm->remote_call = {}; vm->read_contract = {}; }
        }
        else if(op == "signature") {
            skey_t skey; skey[31] = 1;
            hash_t hash; hash[0] = 42;
            const pubkey_t key(skey);
            const auto sig = signature_t::sign(skey, hash);
            vm->write(1, *to_binary(hash)); vm->write(2, *to_binary(key)); vm->write(3, *to_binary(sig));
            return raw(vm->read(1)) + "," + raw(vm->read(2)) + "," + raw(vm->read(3));
        }
        else if(op == "effects") {
            std::string out = effects;
            for(const auto& v : vm->outputs) out += "S," + raw(to_binary(v.contract).get()) + "," + raw(to_binary(v.address).get()) + "," + v.amount.str(10) + "," + (v.memo ? raw(binary_t::alloc(*v.memo).get()) : "-") + ";";
            for(const auto& v : vm->mint_outputs) out += "M," + raw(to_binary(v.contract).get()) + "," + raw(to_binary(v.address).get()) + "," + v.amount.str(10) + "," + (v.memo ? raw(binary_t::alloc(*v.memo).get()) : "-") + ";";
            return out.empty() ? "-" : out;
        }
        else if(op == "init") vm->init();
        else if(op == "budget") vm->gas_limit = vm->gas_used + address(f[1]);
        else if(op == "write") vm->write(address(f[1]), *value(f[2]));
        else if(op == "assign") vm->assign(address(f[1]), value(f[2]));
        else if(op == "alloc") { const auto a = vm->alloc(); aliases[f[1]] = a; return std::to_string(a); }
        else if(op == "lookup") return std::to_string(vm->lookup(*value(f[2]), f[1] == "1"));
        else if(op == "write_entry") vm->write_entry(address(f[1]), address(f[2]), *value(f[3]));
        else if(op == "write_key") vm->write_key(address(f[1]), *value(f[2]), *value(f[3]));
        else if(op == "push") vm->push_back(address(f[1]), *value(f[2]));
        else if(op == "pop") vm->pop_back(address(f[1]), address(f[2]));
        else if(op == "copy") vm->copy(address(f[1]), address(f[2]));
        else if(op == "clone") vm->clone(address(f[1]), address(f[2]));
        else if(op == "erase") vm->erase(address(f[1]));
        else if(op == "erase_entry") vm->erase_entry(address(f[1]), address(f[2]));
        else if(op == "erase_key") { const auto k = vm->lookup(*value(f[2]), true); if(k) vm->erase_key(address(f[1]), k); }
        else if(op == "read") return raw(vm->read(address(f[1])));
        else if(op == "read_entry") return raw(vm->read_entry(address(f[1]), address(f[2])));
        else if(op == "read_key") return raw(vm->read_key(address(f[1]), *value(f[2])));
        else if(op == "commit") vm->commit();
        else if(op == "durable") { vm.reset(); database->commit(address(f[1])); }
        else if(op == "revert") { vm.reset(); database->revert(address(f[1])); }
        else if(op == "run") run(f[1]);
        else throw std::logic_error("bad fixture command: " + command);
        return "-";
    }
};

int main(int argc, char** argv) {
    if(argc == 2 && std::string(argv[1]) == "--collision") {
        secp256k1_init();
        skey_t skey; skey[31] = 1;
        const auto target = uint64_t(vnx::Hash64(signature_t().crc64(), vnx::Hash64(0))) % 16384;
        for(uint64_t i = 1; i < 1000000; ++i) {
            hash_t hash; hash.from_uint(i);
            const auto sig = signature_t::sign(skey, hash);
            if(uint64_t(vnx::Hash64(sig.crc64(), vnx::Hash64(0))) % 16384 == target) {
                std::cout << "b:" << raw(to_binary(hash).get()).substr(10) << '\n'
                    << "b:" << raw(to_binary(pubkey_t(skey)).get()).substr(10) << '\n'
                    << "b:" << raw(to_binary(sig).get()).substr(10) << '\n';
                return 0;
            }
        }
        return 1;
    }
    if(argc != 4) return 1;
    secp256k1_init();
    std::ifstream cases(argv[1]); std::ofstream out(argv[2]);
    std::string name, command; std::unique_ptr<Runner> runner;
    size_t count = 0;
    while(std::getline(cases, command)) {
        if(command.empty() || command[0] == '#') continue;
        if(command[0] == '[') {
            name = command.substr(1, command.size() - 2);
            runner = std::make_unique<Runner>(std::string(argv[3]) + '/' + name);
            continue;
        }
        runner->storage->writes.clear();
        std::string result;
        try { result = runner->execute(command); }
        catch(const std::exception& error) {
            result = "ERR";
            // The real transaction executor discards the storage overlay when
            // commit fails. Do not retain a C++ commit's partial table writes.
            if(command == "commit") runner->database->revert(runner->database->version());
        }
        const auto* vm = runner->vm.get();
        out << name << '\t' << command << '\t' << result << '\t' << (vm ? vm->gas_used : 0)
            << '\t' << (vm ? vm->error_code : 0) << '\t'
            << (vm && vm->error_addr != uint32_t(-1) ? std::to_string(vm->error_addr) : "-") << '\t'
            << (result != "ERR" && !runner->storage->writes.empty() ? runner->storage->writes : "-") << '\n';
        ++count;
    }
    std::cout << "Generated " << count << " composite trace steps\n";
    return cases.eof() && out ? 0 : 1;
}
