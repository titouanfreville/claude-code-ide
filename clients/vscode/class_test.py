from abc import ABC, abstractmethod


class Test(ABC):
    @abstractmethod
    def test_1(): ...

    @abstractmethod
    def test_2():
        pass

    @abstractmethod
    def test_3():
        return "Funky kong"


class Imp(Test):
    __test = True


print(Imp.test_3())
print(Imp.test_2())
print(Imp.test_1())
