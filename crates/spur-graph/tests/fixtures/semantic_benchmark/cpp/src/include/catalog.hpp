#pragma once

#include <string>

namespace bench {

struct Catalog {
    bool initialize(const std::string& path);
    static Catalog load();

    template <typename T>
    static T make();
};

struct CachedCatalog : public Catalog {};

struct Plain {};

}  // namespace bench
