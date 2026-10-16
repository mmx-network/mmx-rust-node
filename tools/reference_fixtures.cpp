// Fixture oracle for the MMX VM; intentionally no VNX object serialization.
#include <mmx/vm/Engine.h>
#include <mmx/vm/StorageRAM.h>
#include <fstream>
#include <iomanip>
#include <sstream>
#include <iostream>

using namespace mmx::vm;

std::string hex(const uint8_t* data, size_t size) {
    std::ostringstream out;
    out << std::hex << std::setfill('0');
    for(size_t i = 0; i < size; ++i) out << std::setw(2) << unsigned(data[i]);
    return out.str();
}

std::string raw(const var_t& value, bool rc = false, bool vf = false) {
    auto data = serialize(value, rc, vf);
    return hex(data.first.get(), data.second);
}

int main(int argc, char** argv) {
    if(argc != 2) return 1;
    const std::string path = argv[1];
    std::ofstream values(path + "/values.tsv");
    std::ofstream engine(path + "/engine.tsv");
    std::ofstream bytecode(path + "/bytecode.hex");
    std::vector<std::unique_ptr<var_t>> vars;
    vars.emplace_back(std::make_unique<var_t>());
    vars.emplace_back(std::make_unique<var_t>(false));
    vars.emplace_back(std::make_unique<var_t>(true));
    vars.emplace_back(std::make_unique<ref_t>(uint64_t(-1)));
    const std::vector<uint256_t> integers = {
        0, 1, 2, 0xFFFF, uint256_t(1) << 16, uint256_t(1) << 32,
        (uint256_t(1) << 64) - 1, uint256_t(1) << 64,
        (uint256_t(1) << 128) - 1, uint256_t(1) << 128,
        (uint256_t(1) << 255) + 1, uint256_max
    };
    for(const auto& i : integers) vars.emplace_back(std::make_unique<uint_t>(i));
    vars.emplace_back(binary_t::alloc(""));
    vars.emplace_back(binary_t::alloc("test"));
    vars.emplace_back(binary_t::alloc("z"));
    vars.emplace_back(binary_t::alloc("aa"));
    const uint8_t bytes[] = {0, 1, 255, 128};
    vars.emplace_back(binary_t::alloc(bytes, sizeof(bytes), TYPE_BINARY));
    vars.emplace_back(binary_t::alloc(nullptr, 0, TYPE_BINARY));
    vars.emplace_back(binary_t::alloc(bytes, sizeof(bytes), TYPE_STRING));
    auto array = std::make_unique<array_t>(uint32_t(-1));
    array->address = 0x123456789ABCDEF0;
    vars.emplace_back(std::move(array));
    auto map = std::make_unique<map_t>();
    map->address = uint64_t(-1);
    vars.emplace_back(std::move(map));
    for(size_t i = 0; i < vars.size(); ++i) {
        vars[i]->ref_count = 0x12345678;
        vars[i]->flags = 0x1F;
        for(bool rc : {false, true}) for(bool vf : {false, true})
            values << i << '\t' << rc << '\t' << vf << '\t' << raw(*vars[i], rc, vf) << '\n';
        vars[i]->flags = 0;
        vars[i]->ref_count = 0;
    }
    std::vector<instr_t> code = {
        {OP_COPY, 0, uint32_t(MEM_STACK), 1},
        {OP_ADD, OPFLAG_CATCH_OVERFLOW, uint32_t(MEM_STATIC), uint32_t(MEM_STACK), 2},
        {OP_JUMPI, OPFLAG_REF_A, 0x7FFF, uint32_t(MEM_EXTERN)},
        {OP_CONV, OPFLAG_REF_C, 255, 256, 0x7FFE, uint32_t(-1)},
        {OP_RET}, {OP_NOP}, {OP_RET}
    };
    const auto data = serialize(code);
    bytecode << hex(data.data(), data.size()) << '\n';

    size_t count = 0;
    const auto test = [&](uint32_t protocol, opcode_e op, uint8_t flags, const var_t& l, const var_t& r) {
        auto backend = std::make_shared<StorageRAM>();
        Engine vm(mmx::addr_t(), backend, false, protocol);
        vm.gas_limit = 10000000;
        vm.write(1, l); vm.write(2, r);
        vm.init();
        vm.code = {{op, flags, uint32_t(MEM_STACK), 1, 2}, {OP_RET}};
        vm.begin(0);
        std::string result;
        try { vm.run(); result = raw(vm.read_fail(MEM_STACK)); }
        catch(const std::exception&) { result = "ERR"; }
        engine << count++ << '\t' << protocol << '\t' << unsigned(op) << '\t' << unsigned(flags)
            << '\t' << raw(l) << '\t' << raw(r) << '\t' << result << '\t' << vm.gas_used << '\n';
    };
    for(uint32_t protocol : {0, 1}) for(uint8_t flags : {uint8_t(0), OPFLAG_CATCH_OVERFLOW})
        for(opcode_e op : {OP_ADD, OP_SUB, OP_MUL, OP_DIV, OP_MOD, OP_MIN, OP_MAX})
            for(const auto& l : integers) for(const auto& r : integers) test(protocol, op, flags, uint_t(l), uint_t(r));
    for(opcode_e op : {OP_CMP_EQ, OP_CMP_NEQ, OP_CMP_LT, OP_CMP_GT, OP_CMP_LTE, OP_CMP_GTE, OP_NOT, OP_XOR, OP_AND, OP_OR, OP_TYPE, OP_SIZE})
        for(const auto& l : vars) for(const auto& r : vars) {
            if(l->type == TYPE_REF || l->type == TYPE_ARRAY || l->type == TYPE_MAP ||
               r->type == TYPE_REF || r->type == TYPE_ARRAY || r->type == TYPE_MAP) continue;
            test(1, op, 0, *l, *r);
        }
    for(opcode_e op : {OP_NOT, OP_XOR, OP_AND, OP_OR})
        for(const auto& l : integers) for(const auto& r : integers) test(1, op, OPFLAG_BITWISE, uint_t(l), uint_t(r));
    for(opcode_e op : {OP_SHL, OP_SHR}) for(const auto& l : integers)
        for(uint64_t shift : {0, 1, 63, 64, 127, 128, 255, 256, 257}) test(1, op, OPFLAG_REF_C, uint_t(l), uint_t(shift));
    std::cout << "Generated " << count << " C++ execution cases\n";
    return values && engine && bytecode ? 0 : 1;
}
