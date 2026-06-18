#include "include/catalog.hpp"

#include <algorithm>
#include <vector>

using bench::CachedCatalog;
using bench::Catalog;

namespace custom {
template <typename It, typename Fn>
void for_each(It first, It last, Fn fn) {}
}  // namespace custom

template <typename T>
T make() {
    return T {};
}

bool helper() {
    return true;
}

bool keep_entry(int value) {
    return value > 0;
}

bool outside_std(int value) {
    return value > 1;
}

bool run_catalog(std::vector<int>& values, const std::string& path) {
    Catalog catalog;
    catalog.initialize(path);
    helper();
    Catalog::load();
    make<CachedCatalog>();
    std::find_if(values.begin(), values.end(), keep_entry);
    custom::for_each(values.begin(), values.end(), outside_std);
    return true;
}
