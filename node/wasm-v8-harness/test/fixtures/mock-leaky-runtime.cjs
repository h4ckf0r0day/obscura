module.exports = {
  createRuntime() {
    return {
      evaluate() {
        return 42;
      },
    };
  },
};
