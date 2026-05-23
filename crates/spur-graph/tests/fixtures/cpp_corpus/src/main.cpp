#include "include/catalog.hpp"

#include <iostream>

using spurtest::Catalog;
using spurtest::CatalogEntry;

namespace {

int RunCatalog(const std::string& path) {
    Catalog catalog;
    if (!catalog.Initialize(path)) {
        return 1;
    }
    auto* entry = catalog.GetEntry("default");
    return entry ? 0 : 2;
}

}  // namespace

int main(int argc, char** argv) {
    std::string path = (argc > 1) ? argv[1] : "/tmp/spur.db";
    return RunCatalog(path);
}
